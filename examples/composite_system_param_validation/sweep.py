#!/usr/bin/env python3
"""Run and graph the composite-SystemParam preparation validation matrix."""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import BLACK, BLUE, GRAY, GREEN, RED, Canvas  # noqa: E402


def main() -> None:
    result = subprocess.run(
        ["cargo", "run", "--example", "composite_system_param_validation"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=True,
    )
    rows = re.findall(r"^(\w+): expected=(\w+) actual=(\w+) pass=(true|false)$", result.stdout, re.M)
    if len(rows) != 2:
        raise SystemExit(f"could not parse two matrix cases:\n{result.stdout}")
    passed = [row for row in rows if row[3] == "true"]
    if len(passed) != len(rows):
        raise SystemExit(f"FAIL: {len(passed)}/{len(rows)} composite validation checks matched")

    out = HERE / "plots"
    out.mkdir(exist_ok=True)
    canvas = Canvas(900, 410)
    canvas.text(30, 25, "COMPOSITE SYSTEMPARAM PREPARATION", BLACK, 3)
    canvas.text(30, 52, "public scheduler preparation: observed result vs declarative expectation", BLUE, 2)
    left, top, bottom = 65, 105, 320
    canvas.line(left, top, left, bottom, BLACK)
    canvas.line(left, bottom, 855, bottom, BLACK)
    for index, (case, wanted, actual, _) in enumerate(rows):
        x = 165 + index * 330
        color = GREEN if wanted == actual else RED
        canvas.rect(x, 145, x + 170, bottom, color)
        canvas.text(x + 48, 118, "MATCH" if wanted == actual else "MISMATCH", color, 2)
        canvas.text(x, 340, case.replace("_", " "), BLACK, 1)
        canvas.text(x, 358, f"EXPECTED {wanted}", GRAY, 1)
        canvas.text(x, 374, f"OBSERVED {actual}", GRAY, 1)
    canvas.text(30, 395, f"PASS: {len(passed)}/{len(rows)}: enabled runs; disabled reports the nested contract diagnostic", BLACK, 1)
    canvas.save(out / "composite_system_param_validation_matrix.png")
    print(f"VALIDATION: PASS checks={len(passed)}/{len(rows)}")


if __name__ == "__main__":
    main()
