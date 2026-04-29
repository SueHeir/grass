# oscillator_demo

Shared physics for the [coupling examples](../../examples/coupling/).
Two damped harmonic oscillators connected by an interface spring of
stiffness `κ`; each oscillator is its own [`grass_app::App`](../grass_app/).
The example mains contain only the wiring (which sub-Apps to register,
what schedule to run); this crate has the integrator, plugin, the
canonical coupling function, and the final-state helpers.

## Surface

| item | what it does |
|---|---|
| [`OscillatorPlugin`](src/lib.rs) | adds the integrator system + `OscState` / `OtherX` / `OscParams` / `StepSize` resources to one App. Reads `[oscillator]` from the App's [`Config`](../grass_io/) — pre-seed the App's `Config` resource (via `InputPlugin` on the main App, or `Config::for_subapp` on a sub-App) before adding this plugin. |
| [`OscillatorConfig`](src/lib.rs) | TOML schema struct: `x0` / `v0` / `other_x0` / `k_self` / `gamma` / `k_couple` / `mass` / `dt`. All fields default to 0. |
| [`exchange_positions`](src/lib.rs) | the cross-namespace coupling system used **verbatim** by every coupled example. Reads each side's `OscState.x`, writes it into the other side's `OtherX`. |
| [`extract_final_state`](src/lib.rs) | pulls `OscState` out of both sub-Apps after `start()`; produces `FinalState` with bit-exact `RESULT_BITS` print. |
| [`A`](src/lib.rs) / [`B`](src/lib.rs) | namespace marker types (via `namespace!`) for the two oscillators — `A::NAME = "a"`, `B::NAME = "b"`. |
| `OuterIterStopPlugin` | re-exported from [`grass_multi`](../grass_multi/) for users who want fixed-iter termination outside the standard `RunPlugin` flow. |

## TOML shape

For a single-App use:

```toml
[oscillator]
x0 = 1.0
v0 = 0.0
other_x0 = 0.0
k_self = 1.0
gamma = 0.05
k_couple = 0.0
mass = 1.0
dt = 5.0e-3
```

For a coupled two-oscillator use, namespace it under `[a.*]` /
`[b.*]` and let parent-side
[`Config::for_subapp`](../grass_io/src/config.rs) deliver each
sub-App's slice:

```toml
[a.oscillator]
# ...
[b.oscillator]
# ...
```

## See also

- [`examples/coupling/`](../../examples/coupling/) — the worked tour
  through every coupling regime (explicit / implicit / adaptive / MPI).
- [`grass_io`](../grass_io/) — the `Config` / `InputPlugin` / `RunPlugin`
  machinery `OscillatorPlugin` builds on.
