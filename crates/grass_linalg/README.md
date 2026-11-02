# grass_linalg

Matrix-free iterative solvers shared by every GRASS physics tier: conjugate
gradient, preconditioned conjugate gradient, and Picard/Richardson iteration.

The solvers know nothing about meshes, particles, or equations. A caller supplies
a `LinearOperator` (`y = A x` on a slice) and, optionally, a `Preconditioner`.
The one thing a distributed Krylov solve needs from the parallel runtime is a
global sum, so every solver takes a `grass_mpi::CommBackend` and all-reduces its
inner products through it. Pass `SingleProcessComm` for a serial run.

**Layout contract:** under MPI each rank's slices hold its own rows. Any entry
this rank does not own (ghost or halo padding) must stay zero in `rhs`, in the
operator's output, and in the initial guess, so it never enters a dot product.
Filling ghosts before reading a neighbour's value is the operator's job (FIELD's
halo exchange, SOIL's ghost particles, ...).

| Item | Purpose |
|---|---|
| `LinearOperator` | `dimension()` + `apply(x, y)` |
| `Preconditioner` | `apply(r, z)`, `z ≈ M⁻¹ r` (e.g. a multigrid V-cycle) |
| `conjugate_gradient` | CG for SPD operators |
| `preconditioned_conjugate_gradient` | PCG with an SPD preconditioner |
| `picard_iteration` | `x ← x + ω (b − A x)` |
