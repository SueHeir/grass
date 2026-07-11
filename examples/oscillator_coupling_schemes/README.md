# Oscillator coupling schedules

This runnable strong-coupling case holds the oscillator library, physical
parameters, and typed `PositionPort` payload fixed.  Only the parent policy
changes: explicit CSS, Picard with state rollback, under-relaxed Picard, and
retrying Picard that halves its attempted coupling window.

```bash
cargo run --example oscillator_coupling_schemes
python3 examples/oscillator_coupling_schemes/sweep.py
```

The interface spring is deliberately strong (`k_c=600`).  Picard reports its
raw interface residual and rolls back to the start of every attempted window.
The adaptive policy begins at `0.032`; its unrelaxed Picard attempt exceeds the
configured iteration cap, halves until the accepted `0.001` window, then keeps
that accurate window. The reference is a simultaneous (monolithic)
semi-implicit update at `h=0.000025`, with a further refinement check described
in [data/reference.md](data/reference.md).
The executable prints residuals, inner iterations, rejected attempts, accepted
dt, energy ratio, bit-level final fingerprints, and reference error.

![Measured error to the refined monolithic reference and final energy ratio for each policy](plots/coupling_schemes.png)

The graph is generated from the actual executable output. PASS requires the
reference refinement to converge, Picard and relaxed Picard to match the
same-window monolithic solve to `2e-4`, their refined-reference errors to stay
below `0.35`, relaxation to reduce the iteration count, and the adaptive
reduction branch to run. Explicit CSS must retain a visible same-window
interface-lag error.
