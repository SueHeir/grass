#!/usr/bin/env python3
"""Run matrix_free_poisson_cg and plot the MMS convergence check."""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
PLOTS = HERE / "plots"
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import BLACK, BLUE, Canvas, GRAY, GREEN, RED, y_linear  # noqa: E402


def main() -> None:
    result = subprocess.run(
        ["cargo", "run", "--example", "matrix_free_poisson_cg"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=True,
    )
    rows = [
        (int(n), float(h), float(l2))
        for n, h, l2 in re.findall(r"n=\s*([0-9]+), h=([0-9.eE+-]+).* L2=([0-9.eE+-]+)", result.stdout)
    ]
    orders = [float(order) for order in re.findall(r"observed order from previous grid: ([0-9.eE+-]+)", result.stdout)]
    if len(rows) < 3 or not orders:
        raise SystemExit(f"could not parse matrix_free_poisson_cg output:\n{result.stdout}")
    observed = orders[-1]
    passed = all(rows[i + 1][2] < rows[i][2] for i in range(len(rows) - 1)) and observed > 1.95

    hs = [row[1] for row in rows]
    errors = [row[2] for row in rows]
    reference = [errors[0] * (h / hs[0]) ** 2 for h in hs]

    PLOTS.mkdir(exist_ok=True)
    c = Canvas(900, 390)
    c.text(30, 24, "POISSON CG VALIDATION PASS" if passed else "POISSON CG VALIDATION FAIL", BLACK, 3)
    left, top, bottom = 80, 85, 320
    c.text(left, top - 28, "L2 ERROR VS H", BLACK, 2)
    c.line(left, top, left, bottom, BLACK)
    c.line(left, bottom, 500, bottom, BLACK)
    xs = [left + 60 + i * 140 for i in range(len(hs))]
    emax = errors[0] * 1.15
    points = [(x, y_linear(err, emax, top, bottom)) for x, err in zip(xs, errors)]
    ref_points = [(x, y_linear(err, emax, top, bottom)) for x, err in zip(xs, reference)]
    for pair in zip(points, points[1:]):
        c.line(pair[0][0], pair[0][1], pair[1][0], pair[1][1], BLUE)
    for pair in zip(ref_points, ref_points[1:]):
        c.line(pair[0][0], pair[0][1], pair[1][0], pair[1][1], GRAY)
    for x, y in points:
        c.rect(x - 6, y - 6, x + 7, y + 7, BLUE)
    c.text(140, bottom + 18, "MEASURED", BLUE, 2)
    c.text(300, bottom + 18, "O H2", GRAY, 2)

    left = 630
    c.text(left - 20, top - 28, "ORDER", BLACK, 2)
    c.line(left, top, left, bottom, BLACK)
    c.line(left, bottom, 840, bottom, BLACK)
    ymax = max(2.1, observed + 0.05)
    y_obs = y_linear(observed, ymax, top, bottom)
    y_lim = y_linear(1.95, ymax, top, bottom)
    c.rect(705, y_obs, 775, bottom, GREEN)
    c.line(665, y_lim, 820, y_lim, RED)
    c.text(650, bottom + 18, f"OBS {observed:.3F}", BLACK, 2)
    c.text(675, y_lim - 20, "LIMIT 1.95", RED, 2)
    c.save(PLOTS / "poisson_convergence.png")
    print(f"PASS={passed} observed_order={observed:.3f} errors={errors}")


if __name__ == "__main__":
    main()
