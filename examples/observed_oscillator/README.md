# observed_oscillator

End-to-end `grass_io` wiring for a simple harmonic oscillator observed through
the full I/O stack. The example reads its declarative [config.toml](config.toml),
runs two `[[run]]` stages, prints terminal columns, and writes JSON dump frames
under `examples/observed_oscillator/out/`.

## Run

```bash
cargo run --example observed_oscillator
```

Generate the typed starter configuration and field reference:

```bash
cargo run --example observed_oscillator -- --generate-config
```

Expected final line:

```text
observed_oscillator: finished at x = ..., v = ...
```

## Configuration-generation check

![Generated configuration field audit](plots/generated_config_coverage.png)

Run the reproducible audit with the build environment's plotting dependency:

```bash
source ~/projects/.build-env
$BENCH_PYTHON sweep.py
```

The figure shows three independent outcomes: Python's standard-library TOML
parser accepts the emitted bytes, the executable accepts that exact file through
its normal CLI/Serde route, and an injected misspelled key is rejected with an
actionable diagnostic. It does not claim that generated comments can encode all
Serde behaviour; flattened maps and custom deserializers remain application
semantics that need hand-written narrative and tests.
