#!/usr/bin/env python3
"""Run hello_app and plot its asserted step-count check."""

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
        ["cargo", "run", "--example", "hello_app"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=True,
    )
    match = re.search(r"hello_app: ran ([0-9]+) steps", result.stdout)
    if match is None:
        raise SystemExit(f"could not parse hello_app output:\n{result.stdout}")
    measured = int(match.group(1))
    expected = 5
    passed = measured == expected

    PLOTS.mkdir(exist_ok=True)
    c = Canvas(640, 360)
    c.text(30, 24, "HELLO APP VALIDATION PASS" if passed else "HELLO APP VALIDATION FAIL", BLACK, 3)
    left, top, bottom = 95, 85, 285
    c.line(left, top, left, bottom, BLACK)
    c.line(left, bottom, 560, bottom, BLACK)
    ymax = 6.0
    for x, value, color, label in [
        (220, expected, GRAY, "EXPECTED"),
        (360, measured, GREEN if passed else RED, "MEASURED"),
    ]:
        y = y_linear(value, ymax, top, bottom)
        c.rect(x, y, x + 85, bottom, color)
        c.text(x - 10, bottom + 18, label, BLUE if label == "MEASURED" else BLACK, 2)
        c.text(x + 28, bottom + 42, str(value), BLACK, 3)
    print(f"PASS={passed} measured_steps={measured} expected_steps={expected}")
    c.save(PLOTS / "hello_app_validation.png")


if __name__ == "__main__":
    main()
