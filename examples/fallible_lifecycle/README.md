# Fallible lifecycle validation

This validation runs each `grass_app` lifecycle assertion as an independent test
command. The figure compares each measured result with its required PASS value:
plugin-group and nested-plugin error propagation, setup and update short-circuit,
cleanup after build, setup, duplicate-registration, missing-dependency, and capability failures, and compatibility with existing plugins and setup systems. A failed
assertion changes only its own measured row to FAIL.

![Fallible lifecycle validation matrix](plots/fallible_lifecycle_matrix.png)

The recorded result is PASS.

## Run

```bash
../../automation/bin/run-bench.sh examples/fallible_lifecycle
```
