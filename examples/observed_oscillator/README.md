# observed_oscillator

End-to-end `grass_io` wiring for a simple harmonic oscillator observed through
the full I/O stack. The example seeds a declarative TOML config in the binary,
runs two `[[run]]` stages, prints terminal columns, and writes JSON dump frames
under `examples/observed_oscillator/out/`.

## Run

```bash
cargo run --example observed_oscillator
```

Expected final line:

```text
observed_oscillator: finished at x = ..., v = ...
```
