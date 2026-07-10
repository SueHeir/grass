#!/usr/bin/env python3
"""Run the fallible configuration matrix and graph returned versus expected cases."""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import BLACK, BLUE, Canvas, GRAY, GREEN, RED  # noqa: E402


def main() -> None:
    result = subprocess.run(
        ["cargo", "run", "--example", "fallible_config_matrix"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=True,
    )
    rows = re.findall(r"^(\w+): expected=(\w+) actual=(\w+) pass=(true|false)$", result.stdout, re.M)
    if len(rows) != 4:
        raise SystemExit(f"could not parse four matrix cases:\n{result.stdout}")
    passed = [row for row in rows if row[3] == "true"]
    if len(passed) != len(rows):
        raise SystemExit(f"FAIL: {len(passed)}/{len(rows)} typed errors matched")

    out = HERE / "plots"
    out.mkdir(exist_ok=True)
    canvas = Canvas(760, 380)
    canvas.text(30, 25, "FALLIBLE CONFIG ERROR MATRIX", BLACK, 3)
    canvas.text(30, 50, "returned ConfigError variant matches declarative expected variant", BLUE, 2)
    left, top, bottom = 70, 100, 300
    canvas.line(left, top, left, bottom, BLACK)
    canvas.line(left, bottom, 710, bottom, BLACK)
    for index, (case, wanted, actual, _) in enumerate(rows):
        x = 105 + index * 150
        color = GREEN if wanted == actual else RED
        canvas.rect(x, 140, x + 95, bottom, color)
        canvas.text(x + 22, 115, "MATCH" if wanted == actual else "MISMATCH", color, 2)
        canvas.text(x, 320, case.replace("_", " "), BLACK, 1)
        canvas.text(x, 338, actual, GRAY, 1)
    canvas.text(30, 365, f"PASS: {len(passed)}/{len(rows)} returned errors match the declarative matrix", BLACK, 2)
    canvas.save(out / "fallible_config_matrix.png")
    print(f"PASS={len(passed) == len(rows)} cases={len(passed)}/{len(rows)}")


if __name__ == "__main__":
    main()
