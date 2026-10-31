"""Two Python heat solvers coupled through grass, on non-matching meshes.

A hot slab (A) and a cold slab (B) touch along the plane z = 0. Each is a
bundle of 1-D conduction columns, one per interface point, and the two
sides discretize the interface differently: A on an 8 x 8 grid of cells,
B on a scattered triangulated surface. Conductivity varies along the
interface, so the mapping matters.

The coupling is Dirichlet-Neumann: B takes the interface temperature from
A and returns the heat flux; A takes the flux and returns a new interface
temperature. With B much more conductive than A this fixed point is
unstable (plain iteration diverges), so each time window is converged with
grass.OuterIteration. Temperature is sampled A -> B (intensive), flux is
sampled B -> A (density).
"""

import numpy as np
from scipy.linalg import solve_banded

import grass

# ---- interface meshes --------------------------------------------------------
A_IF = grass.UniformGrid((0, 0, -0.01), (1, 1, 0.01), (8, 8, 1), basis="trilinear")
rng = np.random.default_rng(3)
pts = np.vstack([rng.random((1500, 2)), [[0, 0], [1, 0], [0, 1], [1, 1]]])
from scipy.spatial import Delaunay  # noqa: E402

tri = Delaunay(pts)
B_IF = grass.TriSurface(np.c_[pts, np.zeros(len(pts))], tri.simplices, dofs="vertex", tolerance=0.02)

xa = A_IF.centroids()
xb = B_IF.centroids()
T_to_B = grass.Map(A_IF, B_IF, kind="intensive")
q_to_A = grass.Map(B_IF, A_IF, kind="density", sampling="average")
area_A, area_B = A_IF.measures() / 0.02, B_IF.measures()  # A's cells are 0.02 thick


def conductivity(x, base):
    return base * (1.0 + 0.5 * np.sin(2 * np.pi * x[:, 0]) * np.cos(np.pi * x[:, 1]))


class Columns:
    """Bundle of 1-D backward-Euler conduction columns of n cells over depth L."""

    def __init__(self, k, rho_c, n, L, t_far, t0):
        self.k, self.rho_c, self.n, self.dx = k, rho_c, n, L / n
        self.t_far = t_far
        self.T = np.full((len(k), n), t0, dtype=float)  # cell 0 touches the interface

    def _solve(self, dt, interface):
        """interface: ("flux", q_in) or ("temp", T_i). Returns new temperatures."""
        n, dx = self.n, self.dx
        out = np.empty_like(self.T)
        for j, k in enumerate(self.k):
            a = k * dt / (self.rho_c * dx * dx)
            ab = np.zeros((3, n))
            rhs = self.T[j].copy()
            ab[1, :] = 1 + 2 * a
            ab[0, 1:] = -a
            ab[2, :-1] = -a
            # far end: fixed temperature half a cell away
            ab[1, -1] = 1 + 3 * a
            rhs[-1] += 2 * a * self.t_far
            kind, val = interface
            if kind == "flux":  # heat flux into the column
                ab[1, 0] = 1 + a
                rhs[0] += val[j] * dt / (self.rho_c * dx)
            else:  # interface temperature half a cell away
                ab[1, 0] = 1 + 3 * a
                rhs[0] += 2 * a * val[j]
            out[j] = solve_banded((1, 1), ab, rhs)
        return out


def run(scheme, windows=10, dt=0.05, omega0=0.05):
    A = Columns(conductivity(xa, 1.0), 1.0, 20, 1.0, t_far=1000.0, t0=600.0)
    B = Columns(conductivity(xb, 20.0), 1.0, 20, 1.0, t_far=300.0, t0=300.0)
    T_if = np.full(len(A_IF), 450.0)
    it = grass.OuterIteration(T_if, scheme=scheme, omega0=omega0, reuse=4, filter=1e-6, tol=1e-4, max_iters=60)
    history, all_converged = [], True
    for _ in range(windows):
        while True:
            T_B = T_to_B(it.input)  # A's interface temperature on B's mesh
            TB_new = B._solve(dt, ("temp", T_B))
            q_B = 2 * B.k * (T_B - TB_new[:, 0]) / B.dx  # flux leaving A into B
            q_A = -q_to_A(q_B)  # flux into A
            TA_new = A._solve(dt, ("flux", q_A))
            T_tilde = TA_new[:, 0] + q_A * A.dx / (2 * A.k)  # A's surface temperature
            it.observe(T_tilde)
            if it.done:
                break
        A.T, B.T = TA_new, TB_new  # accept the window
        history.append(it.iters)
        all_converged &= it.converged
        it.begin_window()
    return history, it.input, T_to_B(it.input), all_converged


if __name__ == "__main__":
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    results = {s: run(s) for s in ["aitken", "iqn-ils"]}
    for s, (hist, *_rest, conv) in results.items():
        print(f"{s:8s} iterations per window: {hist}  converged: {conv}")
    for w in (1.0, 0.05):
        p = run("fixed", windows=1, omega0=w)
        print(f"fixed relaxation omega={w}: {p[0][0]} iterations, converged: {p[3]}")

    qB = np.sin(3 * xb[:, 0]) + 2
    print(f"heat balance of the flux map: {(q_to_A(qB) * area_A).sum() / (qB * area_B).sum() - 1:+.2e}")
    hist, T_A, T_B, _ = results["iqn-ils"]
    fig, ax = plt.subplots(1, 3, figsize=(15, 4.4))
    for s, (h, *_r) in results.items():
        ax[0].plot(range(1, len(h) + 1), h, "o-", label=s)
    ax[0].set_xlabel("time window")
    ax[0].set_ylabel("coupling iterations")
    ax[0].set_title("Iterations per window")
    ax[0].legend()
    vmin, vmax = min(T_A.min(), T_B.min()), max(T_A.max(), T_B.max())
    sc = ax[1].scatter(xa[:, 0], xa[:, 1], c=T_A, s=420, marker="s", vmin=vmin, vmax=vmax, cmap="inferno")
    ax[1].set_title("Interface T on A (8x8 cells)")
    ax[2].tripcolor(pts[:, 0], pts[:, 1], tri.simplices, T_B, shading="gouraud", vmin=vmin, vmax=vmax, cmap="inferno")
    ax[2].triplot(pts[:, 0], pts[:, 1], tri.simplices, lw=0.1, color="w", alpha=0.5)
    ax[2].set_title("Mapped to B (3000 triangles)")
    for a in ax[1:]:
        a.set_aspect("equal")
        a.set_xlim(0, 1)
        a.set_ylim(0, 1)
    fig.colorbar(sc, ax=ax[1:], label="T [K]")
    fig.savefig("coupled_slabs.png", dpi=120, bbox_inches="tight")
    print("wrote coupled_slabs.png")
