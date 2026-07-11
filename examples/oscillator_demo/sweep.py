#!/usr/bin/env python3
"""Run the oscillator checks and plot actual analytical-error evidence."""
import pathlib, re, subprocess, sys
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
print(f"PASS={passed} max_error={error:.3e} limit={limit:.1e}")
