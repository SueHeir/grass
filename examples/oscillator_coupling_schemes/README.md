# Oscillator coupling schedules

This runnable case holds the oscillator library, physical parameters, and typed
`PositionPort` payload fixed. Only the parent orchestration changes: serial
explicit CSS, rollback Picard, fixed under-relaxed Picard, and residual-driven
adaptive Picard retry.

```bash
source ~/projects/.build-env
$BENCH_PYTHON examples/oscillator_coupling_schemes/showcase.py
```

The deliberately strong interface spring (`k_c=2000`) gives the nominal
macro-step fixed-point factor `q=k_c h^2=0.2`. At four macro steps to `t=0.04`,
serial CSS has a visible interface lag: its independently calculated relative
phase-space error is 0.653, above the 0.50 exposure threshold. The rollback
Picard policies are 0.197, below the stated 0.25 physical-accuracy budget.
Those bounds are not a claim of convergence under refinement; they are a
documented accuracy budget for this intentionally coarse demonstration.

The relaxed policy uses a fixed `omega=0.7`, rather than a case-specific
fixed-point cancellation value. It damps the alternating Picard mode and cuts
the observed maximum iteration count from 14 to 13. The adaptive controller
starts at `h=0.02`, rejects that attempt because its residual remains above the
same configured tolerance after 14 iterations, restores both solvers, and
accepts `h=0.01`; it has no separate timestep acceptance gate.

`showcase.py` is the reproducible evidence command. It runs every coupling
policy, the two-thread `LocalTransport` replay, and the two-rank MPI launch
when `mpirun` is available. It writes the executable's complete macro-step
history to [data/coupling_histories.csv](data/coupling_histories.csv), including
trajectory state, interface residual, inner iterations, accepted/rejected dt,
and energy ratio. The scheme sweep independently reconstructs the full
four-state linear ODE with SciPy's matrix exponential; the MPMD sweep compares
the complete local and MPI state vectors with its separately implemented
recurrence. The exact-mode derivation, source, and limitations are in
[data/reference.md](data/reference.md).

![Measured relative error to the independent matrix-exponential reference. Green lines show the 0.25 converged-policy budget and 0.50 CSS-lag exposure threshold; this run passes.](plots/coupling_schemes.png)

![Coupling trajectories, energy, residual history, Picard work, and accepted dt from the executable.](plots/coupling_histories.png)

The history figure is an execution record rather than a gate graphic: it shows
the transient behavior that the independent terminal-state and full-trajectory
checks evaluate.
