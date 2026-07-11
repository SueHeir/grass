#!/usr/bin/env python3
"""Exercise a generated starter file through independent parser and CLI paths."""
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
generated = subprocess.run(
    ["cargo", "run", "--example", "observed_oscillator", "--", "--generate-config"],
    cwd=ROOT, text=True, capture_output=True, check=True,
)

# Python's stdlib TOML implementation is independent of the Rust `toml` crate
# used by Grass.  It catches invalid syntax without reusing the generator's
# own renderer or metadata structures.
with tempfile.TemporaryDirectory(prefix="grass-generated-config-") as tmp:
    config = Path(tmp) / "generated.toml"
    config.write_text(generated.stdout)
    parsed = tomllib.loads(generated.stdout)
    syntax_ok = bool(parsed)

    # The actual executable must accept the exact emitted bytes through its
    # normal InputPlugin/Serde path, not a test-only Config::from_str helper.
    accepted = subprocess.run(
        ["cargo", "run", "--example", "observed_oscillator", "--", str(config)],
        cwd=ROOT, text=True, capture_output=True,
    )
    accepts_generated = accepted.returncode == 0

    # Adversarial mutation: a typo in a built-in, deny_unknown_fields section
    # must be rejected with the offending key in the user-facing diagnostic.
    bad = Path(tmp) / "unknown-field.toml"
    bad.write_text(generated.stdout.replace("start_step = 0", "start_step = 0\nstart_stpe = 7", 1))
    rejected = subprocess.run(
        ["cargo", "run", "--example", "observed_oscillator", "--", str(bad)],
        cwd=ROOT, text=True, capture_output=True,
    )
    unknown_rejected = rejected.returncode != 0 and "unknown field `start_stpe`" in (rejected.stdout + rejected.stderr)

rows = [
    ("Python stdlib TOML parse", syntax_ok),
    ("Grass CLI accepts generated file", accepts_generated),
    ("Grass rejects injected unknown key", unknown_rejected),
]
passed = all(ok for _, ok in rows)

import matplotlib.pyplot as plt

fig, ax = plt.subplots(figsize=(8, 5.2))
labels = [label for label, _ in rows]
values = [1 if ok else 0 for _, ok in rows]
colors = ["#59a14f" if ok else "#e15759" for _, ok in rows]
ax.barh(range(len(rows)), values, color=colors)
ax.set_yticks(range(len(rows)), labels, fontsize=9)
ax.set_xlim(0, 1.15)
ax.set_xticks([0, 1], ["FAIL", "PASS"])
ax.axvline(1, color="#222", linestyle="--", linewidth=1, label="required outcome")
ax.set_title(f"Generated configuration executable contract: {'PASS' if passed else 'FAIL'}")
ax.legend(loc="lower right", fontsize=8)
fig.tight_layout()
(HERE / "plots").mkdir(exist_ok=True)
fig.savefig(HERE / "plots" / "generated_config_coverage.png", dpi=160)
print(
    "generated config executable audit: "
    f"python_toml={syntax_ok} accepts_generated={accepts_generated} "
    f"unknown_rejected={unknown_rejected} "
    f"{'PASS' if passed else 'FAIL'}"
)
sys.exit(0 if passed else 1)
