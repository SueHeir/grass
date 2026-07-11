# Oscillator coupling schedules

This runnable strong-coupling case holds the oscillator library, physical
parameters, and typed `PositionPort` payload fixed.  Only the parent policy
changes: explicit CSS, Picard with state rollback, under-relaxed Picard, and
retrying Picard that halves its attempted coupling window.

```bash
cargo run --example oscillator_coupling_schemes
python3 examples/oscillator_coupling_schemes/sweep.py
```

The interface spring is deliberately strong (`k_c=600`). At the nominal
window (`dt=0.02`), the antisymmetric Picard eigenvalue is `-0.24`: ordinary
Picard needs 16 iterations, while the documented relaxation `ω=1/(1+0.24)`
converges in two. CSS has a large same-window interface-lag error. The adaptive
policy begins at `0.08`; Picard fails its residual cap at `0.08` and `0.04`,
rolls both solvers back, and accepts `0.02` without any separate timestep
gate. The simultaneous (monolithic) semi-implicit update is the policy
reference; its `h=0.000025` refinement is checked against the exact
antisymmetric normal mode. Both are described in [data/reference.md](data/reference.md).
The executable prints residuals, inner iterations, rejected attempts, accepted
dt, energy ratio, bit-level final fingerprints, and reference error.

![Measured error to the same-window monolithic reference and final energy ratio for each policy](plots/coupling_schemes.png)

The graph is generated from the actual executable output. PASS requires the
refined monolithic reference to agree with the exact mode within `0.4`, and
the converged Picard, relaxed Picard, and adaptive policies to match the
same-window monolithic solve within `2e-7`. It also requires relaxation to
reduce the iteration count, genuine residual-driven adaptive rejection, and a
visible CSS interface-lag error.
