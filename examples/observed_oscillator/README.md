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

The figure shows all 12 fields individually, including each generated type and
required/default status. It passes only when the generated output contains the
complete field set and every row has type, status, and source metadata. The
`grass_io` regression suite independently compares the generated fields and
defaults with the typed Serde configuration definitions, including enum choices.
