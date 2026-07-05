#!/usr/bin/env python3
"""Run verlet_minisolver and plot its asserted error checks."""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
PLOTS = HERE / "plots"
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import BLACK, Canvas, GREEN, RED, y_linear  # noqa: E402


def main() -> None:
    result = subprocess.run(
        ["cargo", "run", "--example", "verlet_minisolver"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=True,
    )
    match = re.search(r"x = ([0-9.eE+-]+), v = ([0-9.eE+-]+)", result.stdout)
    if match is None:
        raise SystemExit(f"could not parse verlet_minisolver output:\n{result.stdout}")
    x = float(match.group(1))
    v = float(match.group(2))
    k = 1.0
    x0 = 1.0
    energy = 0.5 * v * v + 0.5 * k * x * x
    energy0 = 0.5 * k * x0 * x0
    errors = [abs(x - x0), abs(v), abs(energy - energy0)]
    limits = [1.0e-2, 1.0e-2, 1.0e-3]
    passed = all(err < limit for err, limit in zip(errors, limits))

    PLOTS.mkdir(exist_ok=True)
    c = Canvas(720, 390)
    c.text(32, 24, "VERLET VALIDATION PASS" if passed else "VERLET VALIDATION FAIL", BLACK, 3)
    left, top, bottom, right = 80, 85, 320, 660
    c.line(left, top, left, bottom, BLACK)
    c.line(left, bottom, right, bottom, BLACK)
    ymax = max(limits) * 1.2
    labels = ["X", "V", "E"]
    for i, (label, err, limit) in enumerate(zip(labels, errors, limits)):
        x0 = left + 95 + i * 160
        y_err = y_linear(err, ymax, top, bottom)
        y_lim = y_linear(limit, ymax, top, bottom)
        c.rect(x0, y_err, x0 + 70, bottom, GREEN)
        c.line(x0 - 15, y_lim, x0 + 85, y_lim, RED)
        c.text(x0 + 22, bottom + 18, label, BLACK, 3)
        c.text(x0 - 12, bottom + 48, f"{err:.1E}", BLACK, 2)
    c.text(430, 82, "RED LIMIT", RED, 2)
    c.save(PLOTS / "verlet_validation.png")
    print(
        f"PASS={passed} x_error={errors[0]:.3e} v_error={errors[1]:.3e} "
        f"energy_error={errors[2]:.3e}"
    )


if __name__ == "__main__":
    main()
