"""Building GRASS Apps from Python, in four parts.

1. One code: resources, phases, systems and the Run plugin.
2. Plugins: a Python plugin, plus the Rust Provenance and Checkpoint
   plugins checkpointing Python state (restart is bit-identical).
3. Changing the schedule: insert, order, gate and remove systems.
4. Two codes at once: a hot slab and a cold slab as child Apps of one
   parent. The cold one is subcycled 4x, and each time window is
   converged implicitly with IQN-ILS. The slabs meet on non-matching
   interface meshes (64 grid cells vs 3000 triangles).

Run:  python app_walkthrough.py
"""

import os
import tempfile

import numpy as np

import grass


def banner(s):
    print(f"\n=== {s} " + "=" * (60 - len(s)))


# ---------------------------------------------------------------------------
banner("1. one code")
# A 1-D explicit diffusion code. Resources are plain Python objects; a
# system's parameter names say which ones it gets.

rod = grass.App(config="[run]\nsteps = 200\n")
rod.add_plugin(grass.plugins.Run())          # [run] steps / stages, ends the run
rod["T"] = np.zeros(21)
rod["params"] = {"r": 0.4, "left": 100.0, "right": 20.0}
rod.add_phases("boundary", "diffuse")        # execution order of phases


def boundary(T, params):
    T[0], T[-1] = params["left"], params["right"]


def diffuse(T, params):
    T[1:-1] += params["r"] * (T[2:] - 2 * T[1:-1] + T[:-2])


rod.add_system(diffuse, "diffuse")
rod.add_system(boundary, "boundary")
for row in rod.schedule():
    print("  ", row)
rod.run()
print("T after 200 steps:", rod["T"][::4].round(1))


# ---------------------------------------------------------------------------
banner("2. plugins")
# A plugin is any object with build(app). It can add resources, systems
# and phases, and declare plugins it requires.


class History:
    """Records the mean of a named array every `every` steps."""

    def __init__(self, field, every=10):
        self.field, self.every = field, every

    def build(self, app):
        app["history"] = []
        field, every = self.field, self.every

        # `res` is the App's full resource table, for systems that look
        # resources up by a name chosen at run time.
        def record(sim, history, res):
            history.append((sim.step, round(float(res[field].mean()), 4)))

        app.add_system(record, "output", run_if=lambda sim: sim.step % every == 0)


out = tempfile.mkdtemp(prefix="grass_py_")


def logistic_app(extra="", steps=40):
    app = grass.App(
        config=f"[run]\nsteps = {steps}\n[checkpoint]\nevery = 10\nkeep = 0\n{extra}",
        output_dir=out,
    )
    app.add_plugin(grass.plugins.Provenance("logistic-demo", "0.1", notes={"case": "chaos"}))
    app.add_plugin(grass.plugins.Checkpoint())
    app.add_plugin(grass.plugins.Run())
    app.add_plugin(History("x", every=10))
    app["x"] = np.linspace(0.1, 0.4, 4)
    app["rng"] = np.random.default_rng(7)
    app.checkpoint("x", "rng")               # pickled into GRASS checkpoint files

    def step(x, rng):
        x[:] = np.clip(3.97 * x * (1 - x) + 1e-3 * rng.random(x.size), 0, 1)

    app.add_system(step, "update")
    return app


full = logistic_app()
full.run()
print("history:", full["history"])
print("checkpoints:", sorted(os.listdir(os.path.join(out, "checkpoints"))))
restart = logistic_app(f'restart = "{out}/checkpoints/checkpoint_0000000020.grass"', steps=20)
restart.run()
print("restart from step 20 matches the full run bit for bit:",
      np.array_equal(restart["x"], full["x"]))
print("provenance.toml:", open(os.path.join(out, "provenance.toml")).read().splitlines()[:3])


# ---------------------------------------------------------------------------
banner("3. changing the schedule")
app = grass.App()
app["log"] = []


def predict(log):
    log.append("predict")


def correct(log):
    log.append("correct")


def legacy_filter(log):
    log.append("legacy")


app.add_system(predict, "step")
app.add_system(correct, "step", after=predict)       # same phase, explicit order
app.add_system(legacy_filter, "step")
app.add_system(lambda log: log.append("limit"), "step", label="limiter",
               before=correct, after=predict)        # insert between two systems
app.add_system(lambda log: log.append("io"), "output", label="io",
               run_if=lambda sim, log: len(log) > 8)  # gated
app.remove_system(legacy_filter)                     # swap out a system
print("schedule:", [s for _, _, s in app.schedule()])
app.run(steps=3)
print("log:", app["log"])


# ---------------------------------------------------------------------------
banner("4. two codes, one parent")


class Slab:
    """Bundle of 1-D backward-Euler conduction columns (one per interface dof)."""

    def __init__(self, k, n, L, t_far, t0):
        self.k, self.n, self.dx, self.t_far = k, n, L / n, t_far
        self.T = np.full((len(k), n), float(t0))  # column 0 touches the interface

    def solve(self, dt, kind, value):
        a = (self.k * dt / self.dx**2)[:, None] * np.ones(self.n)
        lo, up = -a.copy(), -a.copy()
        di = 1 + 2 * a
        rhs = self.T.copy()
        di[:, -1] = 1 + 3 * a[:, -1]
        rhs[:, -1] += 2 * a[:, -1] * self.t_far
        if kind == "flux":                           # heat flux into the column
            di[:, 0] = 1 + a[:, 0]
            rhs[:, 0] += value * dt / self.dx
        else:                                        # interface temperature
            di[:, 0] = 1 + 3 * a[:, 0]
            rhs[:, 0] += 2 * a[:, 0] * value
        # Thomas algorithm, vectorized over columns.
        c, d = np.empty_like(rhs), np.empty_like(rhs)
        c[:, 0], d[:, 0] = up[:, 0] / di[:, 0], rhs[:, 0] / di[:, 0]
        for i in range(1, self.n):
            m = di[:, i] - lo[:, i] * c[:, i - 1]
            c[:, i] = up[:, i] / m
            d[:, i] = (rhs[:, i] - lo[:, i] * d[:, i - 1]) / m
        x = np.empty_like(rhs)
        x[:, -1] = d[:, -1]
        for i in range(self.n - 2, -1, -1):
            x[:, i] = d[:, i] - c[:, i] * x[:, i + 1]
        self.T = x


class SurfaceProbe:
    """Python plugin: records mean interface temperature after every step."""

    def build(self, app):
        app["probe"] = []
        app.add_system(lambda slab, probe: probe.append(float(slab.T[:, 0].mean())),
                       "output", label="probe")


def slab_code(k, t_far, t0, dt, boundary):
    """A self-contained heat code. `boundary` is "flux" (takes a heat flux,
    reports its surface temperature) or "temp" (takes a temperature,
    reports the flux it draws)."""
    app = grass.App()
    app["slab"] = Slab(k, n=20, L=1.0, t_far=t_far, t0=t0)
    app["bc"] = {"dt": dt, "value": np.zeros(len(k)), "out_sum": np.zeros(len(k)), "n": 0}
    app.add_phases("solve", "report", "output")

    def solve(slab, bc):
        slab.solve(bc["dt"], boundary, bc["value"])

    def report(slab, bc):
        if boundary == "temp":   # flux drawn from the other side
            out = 2 * slab.k * (bc["value"] - slab.T[:, 0]) / slab.dx
        else:                    # surface temperature
            out = slab.T[:, 0] + bc["value"] * slab.dx / (2 * slab.k)
        bc["out_sum"] += out
        bc["n"] += 1

    app.add_system(solve, "solve")
    app.add_system(report, "report")
    app.add_plugin(SurfaceProbe())
    return app


# Interface meshes: the hot side on an 8 x 8 grid, the cold side on 3000 triangles.
from scipy.spatial import Delaunay  # noqa: E402

grid = grass.UniformGrid((0, 0, -0.01), (1, 1, 0.01), (8, 8, 1), basis="trilinear")
rng = np.random.default_rng(3)
pts = np.vstack([rng.random((1500, 2)), [[0, 0], [1, 0], [0, 1], [1, 1]]])
surf = grass.TriSurface(np.c_[pts, np.zeros(len(pts))], Delaunay(pts).simplices,
                        dofs="vertex", tolerance=0.02)


def conductivity(x, base):
    return base * (1.0 + 0.5 * np.sin(2 * np.pi * x[:, 0]) * np.cos(np.pi * x[:, 1]))


def coupled_run(scheme, windows=10, dt=0.05, subcycles=4):
    hot = slab_code(conductivity(grid.centroids(), 1.0), t_far=1000.0, t0=600.0,
                    dt=dt, boundary="flux")
    cold = slab_code(conductivity(surf.centroids(), 20.0), t_far=300.0, t0=300.0,
                     dt=dt / subcycles, boundary="temp")

    world = grass.App(config=f"[run]\nsteps = {windows}\n", output_dir=out)
    world.add_plugin(grass.plugins.Run())
    world.add_plugin(grass.plugins.Provenance("hot-cold", "0.1"))
    world.add_app("hot", hot)
    world.add_app("cold", cold)
    world["maps"] = {
        "T": grass.Map(grid, surf, kind="intensive"),
        "q": grass.Map(surf, grid, kind="density", sampling="average"),
    }
    world["coupling"] = grass.OuterIteration(np.full(len(grid), 450.0), scheme=scheme,
                                             omega0=0.05, reuse=4, filter=1e-6,
                                             tol=1e-4, max_iters=60)
    world["iterations"] = []

    def implicit_window(sim, hot, cold, maps, coupling, iterations):
        saved = hot.slab.T.copy(), cold.slab.T.copy()
        while True:
            hot.slab.T, cold.slab.T = saved[0].copy(), saved[1].copy()
            # cold side: interface temperature in, mean flux over its substeps out
            cold.bc["value"] = maps["T"](coupling.input)
            cold.bc["out_sum"][:] = 0
            cold.bc["n"] = 0
            sim.tick("cold", subcycles)
            q_cold = cold.bc["out_sum"] / cold.bc["n"]
            # hot side: flux in, surface temperature out
            hot.bc["value"] = -maps["q"](q_cold)
            hot.bc["out_sum"][:] = 0
            hot.bc["n"] = 0
            sim.tick("hot")
            coupling.observe(hot.bc["out_sum"])
            if coupling.done:
                break
        iterations.append(coupling.iters)
        coupling.begin_window()

    world.add_system(implicit_window, "couple")
    return world, hot, cold


world, hot, cold = coupled_run("iqn-ils")
print("parent schedule:")
for row in world.schedule():
    print("  ", row)
world.run()
print("IQN-ILS iterations per window:", world["iterations"])

aitken, *_ = coupled_run("aitken")
aitken.run()
print("Aitken  iterations per window:", aitken["iterations"])

# The child Apps' resources stay readable from Python after the run.
probe = np.array(cold["probe"])
print(f"cold code ran {len(probe)} substeps in total "
      f"({len(world['iterations'])} windows x 4 substeps x coupling iterations)")
print("hot interface T at the end of each window:",
      np.round(np.array(hot["probe"])[np.cumsum(world["iterations"]) - 1], 1))
