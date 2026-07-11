#!/usr/bin/env python3
"""Generate the committed local-contract comparison plot."""
import re
import subprocess
from pathlib import Path
import matplotlib.pyplot as plt

root = Path(__file__).resolve().parents[2]
out = subprocess.check_output(
    ["cargo", "run", "--quiet", "--example", "oscillator_mpmd_local"], cwd=root, text=True
)
line = next(x for x in out.splitlines() if x.startswith("LOCAL "))
match = re.search(r"a=([^ ]+) b=([^ ]+) mirrors=([^ ]+)", line)
a = [float(x) for x in match.group(1).split(",")]
b = [float(x) for x in match.group(2).split(",")]
mirrors = [float(x) for x in match.group(3).split(",")]
# The remote mirror is the in-process counterpart's exchange observation:
# A's received export must equal B's local x and conversely.  This checks the
# actual send/receive contract rather than a decorative trajectory plot.
reference = [b[0], a[0]]
measured = mirrors
error = max(abs(a-b) for a, b in zip(measured, reference))
assert error < 5e-14, error

labels = ["A mirror ← B.x", "B mirror ← A.x"]
fig, ax = plt.subplots(figsize=(7, 3.6))
ax.plot(labels, reference, "o", label="independent recurrence")
ax.plot(labels, measured, "x", ms=9, mew=2, label="LocalTransport replay")
ax.set_ylabel("received/exported position")
ax.set_title(f"two-sided exchange contract: max |difference| = {error:.1e} (pass < 5e-14)")
ax.grid(axis="y", alpha=.3)
ax.legend()
fig.tight_layout()
plot = Path(__file__).with_name("plots") / "local_contract_comparison.png"
plot.parent.mkdir(exist_ok=True)
fig.savefig(plot, dpi=160)
print(f"PASS local contract max_abs_error={error:.3e}; wrote {plot}")
