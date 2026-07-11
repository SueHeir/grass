#!/usr/bin/env python3
"""Audit every emitted generated-config field against its typed metadata."""
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
# The typed derive emits one comment followed by its TOML default (or a
# required placeholder).  Check each field, rather than a count, so a missing,
# renamed, malformed, or metadata-free field produces a visible failed row.
comments = [line[2:] for line in result.stdout.splitlines() if line.startswith("# ") and " Source: " in line]
rows = []
for comment in comments:
    source = re.search(r"Source: ([^ ]+)$", comment)
    type_match = re.search(r" Type: ([^.]+)\. ", comment)
    status = re.search(r"(Required\.|Optional; default: .+?\.)", comment)
    field = source.group(1).rsplit(".", 1)[-1] if source else "<missing source>"
    valid = bool(source and type_match and status)
    rows.append((field, valid, type_match.group(1) if type_match else "missing", status.group(1) if status else "missing"))

# Four built-ins contribute 2 + 3 + 2 + 5 fields. The field-level metadata
# assertion in grass_io independently checks their typed Serde contract.
expected_fields = {
    "start_step", "start_time", "every", "columns", "width", "interval",
    "path_template", "name", "steps", "dt", "skip", "save_at_end",
}
emitted_fields = {field for field, *_ in rows}
missing = expected_fields - emitted_fields
unexpected = emitted_fields - expected_fields
passed = not missing and not unexpected and len(rows) == len(expected_fields) and all(row[1] for row in rows)

import matplotlib.pyplot as plt

fig, ax = plt.subplots(figsize=(8, 5.2))
labels = [f"{field}: {ty}, {status}" for field, _, ty, status in rows]
values = [1 if valid else 0 for _, valid, _, _ in rows]
colors = ["#59a14f" if valid else "#e15759" for _, valid, _, _ in rows]
ax.barh(range(len(rows)), values, color=colors)
ax.set_yticks(range(len(rows)), labels, fontsize=7)
ax.set_xlim(0, 1.15)
ax.set_xticks([0, 1], ["mismatch", "metadata present"])
ax.axvline(1, color="#222", linestyle="--", linewidth=1, label="PASS: each typed field has type/status/source")
ax.set_title(f"Generated config field-by-field audit: {'PASS' if passed else 'FAIL'}")
ax.legend(loc="lower right", fontsize=8)
fig.tight_layout()
(HERE / "plots").mkdir(exist_ok=True)
fig.savefig(HERE / "plots" / "generated_config_coverage.png", dpi=160)
print(
    "generated config field audit: "
    f"expected={len(expected_fields)} emitted={len(rows)} "
    f"missing={sorted(missing)} unexpected={sorted(unexpected)} "
    f"{'PASS' if passed else 'FAIL'}"
)
sys.exit(0 if passed else 1)
