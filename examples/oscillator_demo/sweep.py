#!/usr/bin/env python3
"""Run the oscillator checks and plot actual analytical-error evidence."""
import pathlib, re, subprocess, sys, tempfile, tomllib
ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import BLACK, BLUE, GREEN, LIGHT_GREEN, RED, Canvas, y_linear

out = subprocess.run(["cargo", "test", "--example", "oscillator_demo", "--", "--nocapture"], cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=True).stdout
m = re.search(r"uncoupled max_error=([0-9.eE+-]+)", out)
if not m: raise SystemExit(out)
error = float(m.group(1)); limit = 1e-4; passed = error < limit
(HERE / "plots").mkdir(exist_ok=True)
c = Canvas(700, 390); c.text(35, 30, "OSCILLATOR ANALYTICAL CHECK PASS" if passed else "OSCILLATOR ANALYTICAL CHECK FAIL", BLACK, 3)
left, top, bottom = 100, 100, 310
c.line(left, top, left, bottom, BLACK); c.line(left, bottom, 620, bottom, BLACK)
scale = limit * 1.2; band = y_linear(limit, scale, top, bottom); measured = y_linear(error, scale, top, bottom)
c.rect(180, band, 510, bottom, LIGHT_GREEN); c.line(160, band, 560, band, RED); c.rect(320, measured, 410, bottom, BLUE)
c.text(180, bottom + 25, "GREEN: ERROR < 1E-4", GREEN, 2); c.text(280, bottom + 50, f"MEASURED {error:.2E}", BLACK, 2)
c.save(HERE / "plots" / "oscillator_analytical_validation.png")

# Exercise the generated metadata through independent TOML parsing and the
# current two-subapp executable, not a test-only configuration helper.
generated = subprocess.run(
    ["cargo", "run", "--example", "oscillator_demo", "--", "--generate-config"],
    cwd=ROOT, text=True, capture_output=True, check=True,
).stdout
with tempfile.TemporaryDirectory(prefix="grass-oscillator-generated-") as tmp:
    config = pathlib.Path(tmp) / "generated.toml"
    config.write_text(generated)
    syntax_ok = bool(tomllib.loads(generated))
    accepted = subprocess.run(
        ["cargo", "run", "--example", "oscillator_demo", "--", str(config)],
        cwd=ROOT, text=True, capture_output=True,
    )
    accepts_generated = accepted.returncode == 0
    bad = pathlib.Path(tmp) / "unknown.toml"
    bad.write_text(generated.replace("x0 = 1.0", "x0 = 1.0\nx_typo = 2.0", 1))
    rejected = subprocess.run(
        ["cargo", "run", "--example", "oscillator_demo", "--", str(bad)],
        cwd=ROOT, text=True, capture_output=True,
    )
    unknown_rejected = rejected.returncode != 0 and "unknown field `x_typo`" in (rejected.stdout + rejected.stderr)

config_rows = [("Python TOML parser", syntax_ok), ("current oscillator_demo accepts generated config", accepts_generated), ("unknown oscillator key rejected", unknown_rejected)]
config_passed = all(ok for _, ok in config_rows)
c = Canvas(700, 390); c.text(35, 30, "GENERATED CONFIG CONTRACT PASS" if config_passed else "GENERATED CONFIG CONTRACT FAIL", BLACK, 3)
for i, (label, ok) in enumerate(config_rows):
    y = 105 + i * 70
    c.rect(90, y, 580 if ok else 240, y + 35, GREEN if ok else RED)
    c.text(105, y + 11, label, BLACK, 2)
    c.text(600, y + 11, "PASS" if ok else "FAIL", GREEN if ok else RED, 2)
c.text(90, 340, "Each row must pass; generated tables retain A./B. sub-app namespaces.", BLACK, 1)
c.save(HERE / "plots" / "generated_config_contract.png")
print(f"PASS={passed and config_passed} max_error={error:.3e} limit={limit:.1e} toml={syntax_ok} accepts_generated={accepts_generated} unknown_rejected={unknown_rejected}")
sys.exit(0 if passed and config_passed else 1)
