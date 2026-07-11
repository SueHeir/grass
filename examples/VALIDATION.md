# Example Validation

Validated examples commit their result plots beside the runnable example so they
render in Gitea:

- [`oscillator_demo`](oscillator_demo/README.md) embeds
  [`plots/oscillator_analytical_validation.png`](oscillator_demo/plots/oscillator_analytical_validation.png),
  comparing an uncoupled numerical oscillator with its analytical solution.

- [`oscillator_coupling_schemes`](oscillator_coupling_schemes/README.md) embeds
  [`plots/coupling_schemes.png`](oscillator_coupling_schemes/plots/coupling_schemes.png),
  comparing explicit, Picard, relaxed, and adaptive exchange policies with a
  refined monolithic reference.

- [`heat_diffusion_1d`](heat_diffusion_1d/README.md) embeds
  [`plots/heat_diffusion_validation.png`](heat_diffusion_1d/plots/heat_diffusion_validation.png).
- [`hello_app`](hello_app/README.md) embeds
  [`plots/hello_app_validation.png`](hello_app/plots/hello_app_validation.png).
- [`verlet_minisolver`](verlet_minisolver/README.md) embeds
  [`plots/verlet_validation.png`](verlet_minisolver/plots/verlet_validation.png).
- [`matrix_free_poisson_cg`](matrix_free_poisson_cg/README.md) embeds
  [`plots/poisson_convergence.png`](matrix_free_poisson_cg/plots/poisson_convergence.png).
- [`fallible_config_matrix`](fallible_config_matrix/README.md) embeds
  [`plots/fallible_config_matrix.png`](fallible_config_matrix/plots/fallible_config_matrix.png),
  checking typed errors from malformed and missing configuration paths.
- [`typed_system_labels`](typed_system_labels/README.md) embeds
  [`plots/typed_system_labels_matrix.png`](typed_system_labels/plots/typed_system_labels_matrix.png),
  checking typed ordering keys, required-target diagnostics, optional ordering, and legacy string
  compatibility.
