# Non-Particle Solvers on GRASS

GRASS claims to be agnostic to the *entire* discretization paradigm: nothing in
the framework knows about particles, atoms, neighbors, positions, or pairwise
interaction. That is easy to assert and easy to get wrong, so this page is the
evidence — two solvers that carry **no particle state at all**, one of them an
*implicit global solve*, both validated against theory.

For a long time these docs carried a caveat: that GRASS was "not proven for
implicit global solvers (FEM, spectral, Newton–Krylov)". That caveat has now been
retired by construction. Here is what replaced it.

## A mesh solver: 1-D heat diffusion

[`heat_diffusion_1d`](https://github.com/SueHeir/grass) (in
`crates/grass_scheduler/examples`) is a structured-grid **finite-difference**
solver of `∂T/∂t = α ∂²T/∂x²`. There is no particle, atom, neighbor list,
position, or pairwise interaction anywhere in it — only a field sampled on a
fixed grid and a local stencil. The FTCS update is expressed as two ordered
scheduler phases (`Stencil` → `Commit`, the second swapping a double buffer),
riding the same scheduler that hosts the Verlet particle mini-solver.

Two analytical checks gate it:

- **Steady state.** With fixed (Dirichlet) end temperatures the field relaxes to
  the exact linear profile `T(x) = T_L + (T_R − T_L)·x/L`; max deviation asserted
  `< 1e-6`.
- **Fourier-mode decay.** With homogeneous ends, an initial `sin(πx/L)` mode
  decays at the continuum rate `exp(−α (π/L)² t)`, tracked to within the scheme's
  `O(Δx²)` truncation error.

```console
cargo run  -p grass_scheduler --example heat_diffusion_1d
cargo test -p grass_scheduler --example heat_diffusion_1d
```

That covers the *explicit mesh* case. The harder claim is implicit.

## The implicit global-solve proof: FEM Poisson

[`fem_poisson`](https://github.com/SueHeir/field) (in the FIELD repo's
`examples/`) solves steady-state Poisson

```
-∇²u = f    on Ω = (0,1)²
   u = 0    on ∂Ω
```

with continuous **P1 (linear-triangle) finite elements**, as a **single global
sparse linear solve** `K u = b` — no timestepping, no pseudo-time loop. This is
exactly the shape the caveat said was unproven: every unknown couples to every
other through one matrix.

It maps onto the stack like this:

| Concern | Mechanism |
|---|---|
| Mesh geometry & node/element layout | FIELD `UniformMesh` |
| Schedule | a custom `Assemble → Solve → Validate` set — the "assemble → linear-solve → converge" anatomy an implicit solver defines for itself |
| Assemble | a GRASS system loops the mesh cells (each split into two P1 triangles) and scatters the element stiffness + consistent load into a sparse `K`, `b` |
| Solve | one GRASS system: COO → CSC → sparse Cholesky (`nalgebra-sparse`) → back-substitution — a **single** global solve |
| Drive | `app.prepare(); app.run();` — the update schedule runs **exactly once** |

Validated by the method of manufactured solutions (`u = sin(πx) sin(πy)`). P1
elements are second-order in L², and the observed convergence tracks theory:

```
  n   16 ->   32:  observed L2 order p = 1.989
  n   32 ->   64:  observed L2 order p = 1.997
  n   64 ->  128:  observed L2 order p = 1.999
  mean observed order = 1.995   (theory 2.000)
```

```console
# in the field repo:
python3 examples/fem_poisson/sweep.py     # PASS/FAIL convergence gate
```

The point is not that GRASS ships a finite-element library — it does not. The
point is that an FEM solver assembling a sparse matrix and factoring it lives
**as an ordinary example on top of the existing framework**, using only the
scheduler, resources, and mesh the docs already promised such a solver would
ride. No substrate `src` was changed to make it fit.

## What GRASS gives you here — and what it doesn't

Honestly scoped, so you know what you're getting:

- **GRASS gives you** the schedule (an implicit solver's `Assemble → Solve →
  Validate` is just a `ScheduleSet`), typed resources for the matrix and vectors,
  and — through FIELD — the mesh and field storage.
- **You bring** the linear algebra. GRASS has no built-in assembler, sparse
  matrix type, or factorization; `fem_poisson` pulls in `nalgebra-sparse` for the
  Cholesky solve. That is deliberate: the framework stays agnostic and you pick
  the solver stack that fits your problem.
- **Still lightly exercised.** The landed proof is one *linear, symmetric,
  single-solve* problem. Nonlinear Newton–Krylov iteration, spectral methods, and
  large distributed sparse solves are consistent with the design but not yet
  demonstrated with a validated example. When they land they will be cited here —
  not before.

## The Eulerian physics tier (in progress)

The particle branch of the stack has a full physics tier — DIRT (DEM) on the SOIL
substrate. The mesh branch has the same shape: **test-cfd**, compressible CFD
(Riemann solvers, EOS, immersed boundaries) expressed as GRASS plugins riding the
FIELD substrate, is the mesh-branch counterpart to DIRT-on-SOIL. It is **in
progress** — planned and being ported, not yet a landed, validated tier — so it
is named here as direction, not as evidence. `fem_poisson` above is the standing
proof that the Eulerian/mesh path through GRASS works; test-cfd is the physics
that will ride it.
