#!/usr/bin/env python3
"""Check the oscillator's generated typed configuration and plot coverage."""
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
result = subprocess.run(
    ["cargo", "run", "--example", "observed_oscillator", "--", "--generate-config"],
    cwd=ROOT, text=True, capture_output=True, check=True,
)
sources = re.findall(r"^# .* Source: crates/grass_io/src/", result.stdout, re.MULTILINE)
expected = 12  # clock(2) + term_out(3) + dump(2) + run stage(5)
emitted = len(sources)
passed = emitted == expected

import matplotlib.pyplot as plt

fig, ax = plt.subplots(figsize=(6, 3.5))
bars = ax.bar(["typed contract", "generated reference"], [expected, emitted],
              color=["#4c78a8", "#59a14f" if passed else "#e15759"])
ax.axhline(expected, color="#222", linestyle="--", linewidth=1, label="PASS criterion")
ax.bar_label(bars, padding=3)
ax.set_ylabel("field count")
ax.set_ylim(0, expected + 3)
ax.set_title(f"Generated config coverage: {'PASS' if passed else 'FAIL'}")
ax.legend()
fig.tight_layout()
(HERE / "plots").mkdir(exist_ok=True)
fig.savefig(HERE / "plots" / "generated_config_coverage.png", dpi=160)
print(f"generated config coverage: expected={expected} emitted={emitted} {'PASS' if passed else 'FAIL'}")
sys.exit(0 if passed else 1)
