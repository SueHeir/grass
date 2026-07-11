# Independent reference and limits

For two equal undamped masses,

`m x_a''=-k x_a-k_c(x_a-x_b)` and `m x_b''=-k x_b-k_c(x_b-x_a)`.

The antisymmetric initial state used here (`x_a=1`, `x_b=-1`, zero velocities)
is the relative normal mode, with angular frequency
`sqrt((k+2 k_c)/m)=sqrt(4001)`. Normal-mode reduction of coupled oscillators is
standard; see A. H. Nayfeh and D. T. Mook, *Nonlinear Oscillations*, Wiley
(1979), §1.4. The validation does not reuse the Rust closed-form helper:
`sweep.py` independently forms the four-by-four first-order system and obtains
the reference with `scipy.linalg.expm`.

The relative phase-space error is `||y-y_ref||_2 / ||y_ref||_2`, including both
positions and velocities. The 0.25 convergence-policy accuracy budget and 0.50
CSS-lag exposure threshold were chosen before the generated run to distinguish
the intended coarse-policy behavior; they are plotted and asserted by both the
executable and independent sweep. They are not fitted references and should
not be read as a general method-order or stability claim.

The relaxation is a fixed under-relaxation `omega=0.7`, not the optimal
case-specific `1/(1+q)`. For the antisymmetric Picard mode its contraction is
`|1-omega(1+q)|=0.16`, compared with `q=0.2` without relaxation. This is the
standard damping rationale for relaxed fixed-point iteration; see C. T. Kelley,
*Iterative Methods for Linear and Nonlinear Equations*, SIAM (1995), §4.4.
The example reports the observed iteration count rather than claiming a
universal speedup.

Limits: the oscillator library integrates each local solver with semi-implicit
Euler, so fixed-point convergence only solves the coupled discrete step. It
does not eliminate time-discretisation error, and this narrow linear,
antisymmetric case does not establish behavior for nonlinear, damped, or
multi-rate scientific models.
