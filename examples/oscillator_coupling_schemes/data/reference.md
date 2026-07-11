# Reference integration

The accuracy reference is exact for this antisymmetric initial condition:
`x_a(0)=1`, `x_b(0)=-1`, and both initial velocities are zero, so only the
relative normal mode is present. Therefore
`x_a=cos(sqrt((k+2k_c)/m)t)`, `v_a=-sqrt((k+2k_c)/m) sin(sqrt((k+2k_c)/m)t)`,
and oscillator B has the opposite state. This follows directly from

`m x_a'' = -k x_a - k_c (x_a-x_b)` and its symmetric partner.

The numerical cross-check advances the two oscillator velocities
simultaneously from the same old state, then advances both positions with
those new velocities. This is the monolithic semi-implicit-Euler
discretization of the same equations.

It uses `h=0.000025`, 40 times smaller than the nominal coupling window.  The
coupled relative mode has angular frequency `sqrt((k+2 k_c)/m)`, so the chosen
strong case (`k=1`, `k_c=600`) is stable at the reference step.  The plotted
reference-discretization refinement is part of the executable check; it is
not fitted to any scheme. The case runs to `t=0.1`: the refined monolithic
solution and each converged policy must be within `0.4` of the exact solution.
This is under 1.2% of the normal mode's velocity scale `sqrt(1201)`, a stated
accuracy budget rather than a refinement-to-refinement gate. The same-window
Picard fixed-point error remains limited to `2e-4`.
