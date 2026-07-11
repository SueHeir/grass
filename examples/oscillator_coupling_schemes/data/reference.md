# Reference integration

The reference advances the two oscillator velocities simultaneously from the
same old state, then advances both positions with those new velocities.  This
is the monolithic semi-implicit-Euler discretization of

`m x_a'' = -k x_a - k_c (x_a-x_b)` and its symmetric partner.

It uses `h=0.000025`, 40 times smaller than the nominal coupling window.  The
coupled relative mode has angular frequency `sqrt((k+2 k_c)/m)`, so the chosen
strong case (`k=1`, `k_c=600`) is stable at the reference step.  The plotted
reference-discretization refinement is part of the executable check; it is
not fitted to any scheme. The case runs to `t=0.1`: the refined-reference
error limits are 0.35 for the three converged Picard policies, while their
same-window monolithic error is limited to `2e-4`. The latter is the policy
criterion; the former records the independent time-discretization error.
