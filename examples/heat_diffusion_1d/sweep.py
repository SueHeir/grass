#!/usr/bin/env python3
"""Run heat_diffusion_1d and plot the asserted analytical checks."""

from __future__ import annotations

import math
import pathlib
import re
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
PLOTS = HERE / "plots"
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import BLACK, BLUE, Canvas, GRAY, GREEN, LIGHT_GREEN, RED, y_linear  # noqa: E402


def main() -> None:
    result = subprocess.run(
        ["cargo", "run", "--example", "heat_diffusion_1d"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=True,
    )
    steady = re.search(r"max\|T . linear\| = ([0-9.eE+-]+)", result.stdout)
    mode = re.search(
        r"numerical=([0-9.eE+-]+),\s+analytical=([0-9.eE+-]+), rel_err=([0-9.eE+-]+)",
        result.stdout,
    )
    if steady is None or mode is None:
        raise SystemExit(f"could not parse heat_diffusion_1d output:\n{result.stdout}")

    max_err = float(steady.group(1))
    numerical = float(mode.group(1))
    analytical = float(mode.group(2))
    rel_err = float(mode.group(3))
    steady_pass = max_err < 1.0e-6
    mode_pass = rel_err < 5.0e-3

    PLOTS.mkdir(exist_ok=True)
    c = Canvas(900, 390)
    c.text(30, 24, "HEAT DIFFUSION VALIDATION PASS" if steady_pass and mode_pass else "HEAT DIFFUSION VALIDATION FAIL", BLACK, 3)

    left, top, bottom = 75, 85, 320
    c.text(left, top - 28, "STEADY", BLACK, 2)
    c.line(left, top, left, bottom, BLACK)
    c.line(left, bottom, 375, bottom, BLACK)
    limit_y = y_linear(1.0, 1.2, top, bottom)
    err_y = y_linear(max_err / 1.0e-6, 1.2, top, bottom)
    c.rect(190, err_y, 255, bottom, BLUE)
    c.line(110, limit_y, 340, limit_y, RED)
    c.text(120, limit_y - 20, "LIMIT 1E-6", RED, 2)
    c.text(160, bottom + 18, "MEASURED", BLACK, 2)
    c.text(120, bottom + 42, f"{max_err:.1E}", BLACK, 2)

    left = 520
    c.text(left, top - 28, "MODE DECAY", BLACK, 2)
    c.line(left, top, left, bottom, BLACK)
    c.line(left, bottom, 840, bottom, BLACK)
    ymax = analytical * 1.08
    tol = 5.0e-3 * analytical
    y_hi = y_linear(analytical + tol, ymax, top, bottom)
    y_lo = y_linear(analytical - tol, ymax, top, bottom)
    c.rect(555, y_hi, 825, y_lo, LIGHT_GREEN)
    for x, value, color in [(620, analytical, GRAY), (735, numerical, GREEN)]:
        y = y_linear(value, ymax, top, bottom)
        c.rect(x, y, x + 65, bottom, color)
    c.text(580, bottom + 18, "ANALYTIC", BLACK, 2)
    c.text(705, bottom + 18, "NUMERIC", BLACK, 2)
    c.text(590, bottom + 42, f"REL {rel_err:.1E}", BLACK, 2)

    print(f"PASS={steady_pass and mode_pass} steady_error={max_err:.3e} mode_rel_err={rel_err:.3e}")
    c.save(PLOTS / "heat_diffusion_validation.png")


if __name__ == "__main__":
    main()
