"""Building GRASS Apps from Python, in four parts.

1. One code: typed resources, phases, systems and the Run plugin.
2. Plugins: a Python plugin, plus the Rust Provenance and Checkpoint
   plugins checkpointing Python state (restart is bit-identical).
3. Changing the schedule: insert, order, gate and remove systems.
4. Two codes at once: a hot slab and a cold slab as child Apps of one
   parent. The cold one is subcycled 4x, and each time window is
   converged implicitly with IQN-ILS. The slabs meet on non-matching
   interface meshes (64 grid cells vs 3000 triangles).

Resources are keyed by their class, as Res<T> is in Rust: a system
parameter annotated `temp: Temperature` receives the Temperature
resource. A missing resource is reported before step 1, and pyright /
mypy check everything inside the systems against the dataclasses
(stubs: python/typings).

Run:  python app_walkthrough.py
"""

import os
import tempfile
from dataclasses import dataclass, field
from typing import Any, Callable, Generic, TypeVar

import numpy as np
import numpy.typing as npt
from scipy.spatial import Delaunay

import grass

Array = npt.NDArray[np.float64]


def banner(s: str) -> None:
    print(f"\n=== {s} " + "=" * (60 - len(s)))


# ---------------------------------------------------------------------------
banner("1. one code")
# A 1-D explicit diffusion code.


@dataclass
class Temperature:
    T: Array


@dataclass
class RodParams:
    r: float
    left: float
    right: float


def boundary(temp: Temperature, p: RodParams) -> None:
    temp.T[0], temp.T[-1] = p.left, p.right


def diffuse(temp: Temperature, p: RodParams) -> None:
    T = temp.T
    T[1:-1] += p.r * (T[2:] - 2 * T[1:-1] + T[:-2])


rod = grass.App(config="[run]\nsteps = 200\n")
rod.add_plugin(grass.plugins.Run())          # [run] steps / stages, ends the run
rod.insert(Temperature(np.zeros(21)))
rod.insert(RodParams(r=0.4, left=100.0, right=20.0))
rod.add_phases("boundary", "diffuse")        # execution order of phases
rod.add_system(diffuse, "diffuse")
rod.add_system(boundary, "boundary")
for row in rod.schedule():
    print("  ", row)
rod.run()
print("T after 200 steps:", rod[Temperature].T[::4].round(1))

# Forgetting a resource is caught before anything runs:
broken = grass.App()
broken.insert(Temperature(np.zeros(5)))
broken.add_system(diffuse)
try:
    broken.run(steps=1)
except KeyError as e:
    print("caught before step 1:", e.args[0].splitlines()[1].strip())


# ---------------------------------------------------------------------------
banner("2. plugins")
# A plugin is any object with build(app). It can add resources, systems
# and phases, and declare plugins it requires.

R = TypeVar("R")


@dataclass
class HistoryLog:
    rows: list[tuple[int, float]] = field(default_factory=list)


class History(Generic[R]):
    """Records `measure(resource)` every `every` steps, for any resource class."""

    def __init__(self, cls: type[R], measure: Callable[[R], float], every: int = 10):
        self.cls, self.measure, self.every = cls, measure, every

    def build(self, app: grass.App) -> None:
        app.insert(HistoryLog())
        measure, every = self.measure, self.every

        def record(sim: grass.Sim, log: HistoryLog, target: Any) -> None:
            log.rows.append((sim.step, round(measure(target), 4)))

        record.__annotations__["target"] = self.cls   # resolved like any annotation
        app.add_system(record, "output", run_if=lambda sim: sim.step % every == 0)


@dataclass
class Population:
    x: Array


def logistic(pop: Population, rng: np.random.Generator) -> None:
    pop.x[:] = np.clip(3.97 * pop.x * (1 - pop.x) + 1e-3 * rng.random(pop.x.size), 0, 1)


out = tempfile.mkdtemp(prefix="grass_py_")


def logistic_app(extra: str = "", steps: int = 40) -> grass.App:
    app = grass.App(
        config=f"[run]\nsteps = {steps}\n[checkpoint]\nevery = 10\nkeep = 0\n{extra}",
        output_dir=out,
    )
    app.add_plugin(grass.plugins.Provenance("logistic-demo", "0.1", notes={"case": "chaos"}))
    app.add_plugin(grass.plugins.Checkpoint())
    app.add_plugin(grass.plugins.Run())
    app.add_plugin(History(Population, lambda p: float(p.x.mean()), every=10))
    app.insert(Population(np.linspace(0.1, 0.4, 4)))
    app.insert(np.random.default_rng(7))          # any class can be a resource type
    app.checkpoint(Population, np.random.Generator)  # pickled into GRASS checkpoints
    app.add_system(logistic)
    return app


full = logistic_app()
full.run()
print("history:", full[HistoryLog].rows)
print("checkpoints:", sorted(os.listdir(os.path.join(out, "checkpoints"))))
restart = logistic_app(f'restart = "{out}/checkpoints/checkpoint_0000000020.grass"', steps=20)
restart.run()
print("restart from step 20 matches the full run bit for bit:",
      np.array_equal(restart[Population].x, full[Population].x))
print("provenance.toml:", open(os.path.join(out, "provenance.toml")).read().splitlines()[:3])


# ---------------------------------------------------------------------------
banner("3. changing the schedule")


@dataclass
class Log:
    items: list[str] = field(default_factory=list)


def predict(log: Log) -> None:
    log.items.append("predict")


def correct(log: Log) -> None:
    log.items.append("correct")


def legacy_filter(log: Log) -> None:
    log.items.append("legacy")


def limiter(log: Log) -> None:
    log.items.append("limit")


def write_output(log: Log) -> None:
    log.items.append("io")


def long_enough(log: Log) -> bool:   # run conditions resolve resources the same way
    return len(log.items) > 8


app = grass.App()
app.insert(Log())
app.add_system(predict, "step")
app.add_system(correct, "step", after=predict)          # same phase, explicit order
app.add_system(legacy_filter, "step")
app.add_system(limiter, "step", before=correct, after=predict)  # insert between two
app.add_system(write_output, "output", run_if=long_enough)       # gated
app.remove_system(legacy_filter)                        # swap out a system
print("schedule:", [s for _, _, s in app.schedule()])
app.run(steps=3)
print("log:", app[Log].items)


# ---------------------------------------------------------------------------
banner("4. two codes, one parent")


class Slab:
    """Bundle of 1-D backward-Euler conduction columns (one per interface dof)."""

    def __init__(self, k: Array, n: int, L: float, t_far: float, t0: float):
        self.k, self.n, self.dx, self.t_far = k, n, L / n, t_far
        self.T: Array = np.full((len(k), n), float(t0))  # column 0 touches the interface

    def solve(self, dt: float, kind: str, value: Array) -> None:
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


@dataclass
class Interface:
    """What a slab code exchanges: the boundary value it is given, and the
    sum of what it reports over the steps since the last reset."""
    kind: str          # "flux": given a heat flux, reports surface temperature
                       # "temp": given a temperature, reports the flux it draws
    dt: float
    value: Array
    out_sum: Array
    n: int = 0

    def reset(self) -> None:
        self.out_sum[:] = 0
        self.n = 0

    def mean_out(self) -> Array:
        return self.out_sum / self.n


@dataclass
class Probe:
    surface_T: list[float] = field(default_factory=list)


def solve(slab: Slab, bc: Interface) -> None:
    slab.solve(bc.dt, bc.kind, bc.value)


def report(slab: Slab, bc: Interface) -> None:
    if bc.kind == "temp":
        out = 2 * slab.k * (bc.value - slab.T[:, 0]) / slab.dx
    else:
        out = slab.T[:, 0] + bc.value * slab.dx / (2 * slab.k)
    bc.out_sum += out
    bc.n += 1


class SurfaceProbe:
    """Python plugin: records the mean interface temperature every step."""

    def build(self, app: grass.App) -> None:
        app.insert(Probe())

        def probe(slab: Slab, p: Probe) -> None:
            p.surface_T.append(float(slab.T[:, 0].mean()))

        app.add_system(probe, "output")


def slab_code(k: Array, t_far: float, t0: float, dt: float, kind: str) -> grass.App:
    """A self-contained heat code."""
    app = grass.App()
    app.insert(Slab(k, n=20, L=1.0, t_far=t_far, t0=t0))
    app.insert(Interface(kind, dt, np.zeros(len(k)), np.zeros(len(k))))
    app.add_phases("solve", "report", "output")
    app.add_system(solve, "solve")
    app.add_system(report, "report")
    app.add_plugin(SurfaceProbe())
    return app


# Child App markers: a parent system annotated `hot: Hot` gets a view of
# that child's resources, and `hot[Slab]` is typed as Slab.
class Hot(grass.AppView):
    pass


class Cold(grass.AppView):
    pass


@dataclass
class Maps:
    T: grass.Map   # hot grid -> cold surface, intensive
    q: grass.Map   # cold surface -> hot grid, density


@dataclass
class Iterations:
    per_window: list[int] = field(default_factory=list)


grid = grass.UniformGrid((0, 0, -0.01), (1, 1, 0.01), (8, 8, 1), basis="trilinear")
rng = np.random.default_rng(3)
pts = np.vstack([rng.random((1500, 2)), [[0, 0], [1, 0], [0, 1], [1, 1]]])
surf = grass.TriSurface(np.c_[pts, np.zeros(len(pts))], Delaunay(pts).simplices,
                        dofs="vertex", tolerance=0.02)


def conductivity(x: Array, base: float) -> Array:
    return base * (1.0 + 0.5 * np.sin(2 * np.pi * x[:, 0]) * np.cos(np.pi * x[:, 1]))


SUBCYCLES = 4


def implicit_window(sim: grass.Sim, hot: Hot, cold: Cold, maps: Maps,
                    coupling: grass.OuterIteration, its: Iterations) -> None:
    saved = hot[Slab].T.copy(), cold[Slab].T.copy()
    while True:
        hot[Slab].T, cold[Slab].T = saved[0].copy(), saved[1].copy()
        # cold side: interface temperature in, mean flux over its substeps out
        cold[Interface].value = maps.T(coupling.input)
        cold[Interface].reset()
        sim.tick(Cold, SUBCYCLES)
        # hot side: flux in, surface temperature out
        hot[Interface].value = -maps.q(cold[Interface].mean_out())
        hot[Interface].reset()
        sim.tick(Hot)
        coupling.observe(hot[Interface].mean_out())
        if coupling.done:
            break
    its.per_window.append(coupling.iters)
    coupling.begin_window()


def coupled_run(scheme: str, windows: int = 10, dt: float = 0.05):
    hot = slab_code(conductivity(grid.centroids(), 1.0), t_far=1000.0, t0=600.0,
                    dt=dt, kind="flux")
    cold = slab_code(conductivity(surf.centroids(), 20.0), t_far=300.0, t0=300.0,
                     dt=dt / SUBCYCLES, kind="temp")

    world = grass.App(config=f"[run]\nsteps = {windows}\n", output_dir=out)
    world.add_plugin(grass.plugins.Run())
    world.add_plugin(grass.plugins.Provenance("hot-cold", "0.1"))
    world.add_app(Hot, hot)
    world.add_app(Cold, cold)
    world.insert(Maps(T=grass.Map(grid, surf, kind="intensive"),
                      q=grass.Map(surf, grid, kind="density", sampling="average")))
    world.insert(grass.OuterIteration(np.full(len(grid), 450.0), scheme=scheme,  # type: ignore[arg-type]
                                      omega0=0.05, reuse=4, filter=1e-6, tol=1e-4, max_iters=60))
    world.insert(Iterations())
    world.add_system(implicit_window, "couple")
    return world, hot, cold


world, hot, cold = coupled_run("iqn-ils")
print("parent schedule:")
for row in world.schedule():
    print("  ", row)
world.run()
print("IQN-ILS iterations per window:", world[Iterations].per_window)

aitken, _, _ = coupled_run("aitken")
aitken.run()
print("Aitken  iterations per window:", aitken[Iterations].per_window)

# The child Apps' resources stay readable from Python after the run.
probe = cold[Probe].surface_T
its = world[Iterations].per_window
print(f"cold code ran {len(probe)} substeps in total "
      f"({len(its)} windows x {SUBCYCLES} substeps x coupling iterations)")
print("hot interface T at the end of each window:",
      np.round(np.array(hot[Probe].surface_T)[np.cumsum(its) - 1], 1))
