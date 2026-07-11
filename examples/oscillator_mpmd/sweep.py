#!/usr/bin/env python3
"""Validate the LocalTransport MPMD replay against an independent recurrence."""
import re
import subprocess
import tomllib
from pathlib import Path

import matplotlib.pyplot as plt

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
TOL = 5e-14
TRACE = re.compile(r"LOCAL_TRACE step=(\d+) a=([^,]+),([^ ]+) b=([^,]+),([^ ]+)")


def recurrence(config):
    """Standalone semi-implicit explicit exchange recurrence.

    This intentionally does not call the Rust library or transport helpers.
    It is the numerical definition of the documented export-after-local-tick
    protocol: both solvers use the previous peer export, then publish their
    just-updated positions for the next step.
    """
    def state(side):
        p = config[side]["oscillator"]
        return [p["x0"], p["v0"], p["peer_x0"], p]

    a, b = state("a"), state("b")
    values = []
    for _ in range(config["case"]["steps"]):
        def tick(q):
            x, v, peer, p = q
            accel = (-p["stiffness"] * x - p["damping"] * v
                     - p["coupling_stiffness"] * (x - peer)) / p["mass"]
            v += accel * p["dt"]
            return [x + v * p["dt"], v, peer, p]
        a_next, b_next = tick(a), tick(b)
        # Setup has already exchanged each side's initial position.  After
        # every completed iteration, each side imports the other side's fresh
        # export for its next local tick.
        a_next[2], b_next[2] = b_next[0], a_next[0]
        a, b = a_next, b_next
        values.append((a[0], a[1], b[0], b[1]))
    return values


def local_trace(output):
    rows = []
    for line in output.splitlines():
        match = TRACE.fullmatch(line)
        if match:
            rows.append(tuple(float(match.group(i)) for i in range(2, 6)))
    return rows


def max_error(actual, expected):
    if len(actual) != len(expected):
        raise AssertionError(f"trace length {len(actual)} != expected {len(expected)}")
    return max(abs(x - y) for row, ref in zip(actual, expected) for x, y in zip(row, ref))


config = tomllib.loads((HERE / "config.toml").read_text())
reference = recurrence(config)
output = subprocess.check_output(
    ["cargo", "run", "--quiet", "--example", "oscillator_mpmd_local"], cwd=ROOT, text=True
)
measured = local_trace(output)
error = max_error(measured, reference)
assert error <= TOL, f"LocalTransport trajectory error {error:.3e} exceeds {TOL:.1e}"

steps = range(1, len(reference) + 1)
fig, axes = plt.subplots(2, 1, figsize=(7.4, 5.4), sharex=True)
for axis, index, name in zip(axes, (0, 2), ("oscillator A position", "oscillator B position")):
    ref = [row[index] for row in reference]
    got = [row[index] for row in measured]
    axis.plot(steps, ref, "-", lw=2, label="independent explicit recurrence")
    axis.plot(steps, got, "x", ms=3.5, label="LocalTransport two-sided replay")
    axis.fill_between(steps, [x - TOL for x in ref], [x + TOL for x in ref],
                      color="C0", alpha=.18, label=f"acceptance band ±{TOL:.0e}")
    axis.set_ylabel(name)
    axis.grid(alpha=.3)
axes[0].legend(loc="best", fontsize=8)
axes[-1].set_xlabel("completed coupling step")
fig.suptitle(f"full 40-step contract trajectory: max |Local − recurrence| = {error:.1e} (PASS)")
fig.tight_layout()
plot = HERE / "plots" / "local_contract_comparison.png"
plot.parent.mkdir(exist_ok=True)
fig.savefig(plot, dpi=160)
print(f"PASS LocalTransport full_trajectory_max_abs_error={error:.3e} tolerance={TOL:.1e}; wrote {plot}")
