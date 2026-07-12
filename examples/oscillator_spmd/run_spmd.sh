#!/usr/bin/env bash
# Prove that a single binary composes the two coupled oscillator roles two ways
# and that *only the declarative TOML* selects which:
#
#   * local.toml  -> both roles in one process (LocalTransport)
#   * split.toml  -> one role per rank under `mpirun -np 2` (MPI)
#
# The Rust source and the physics tables are byte-identical between the runs;
# the only difference the runner introduces is which `[topology]` document each
# launch reads. This is a launcher/checker, not configuration: every physical
# input lives in the TOML files.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
here="examples/oscillator_spmd"

cargo build --quiet --features mpi --example oscillator_spmd
bin=target/debug/examples/oscillator_spmd

# Serial local composition — mode = "local", world size 1.
local=$("$bin" "$here/local.toml")

# MPI role-split composition — mode = "split", one role per rank.
if ! mpi=$(mpirun -np 2 "$bin" "$here/split.toml" 2>&1); then
    printf '%s\n' "$mpi" >&2
    printf '%s\n' "SPMD split launch failed before a complete trace; verify MPI runtime/library compatibility, then that -np matches the split.toml role total (2)." >&2
    exit 1
fi

# Fail-closed guardrails: the declarative mode must reject a wrong launch.
if "$bin" "$here/split.toml" >/dev/null 2>&1; then
    printf '%s\n' "FAIL: split.toml ran serially but should fail closed (world size 1 != 2)" >&2
    exit 1
fi
if mpirun -np 2 "$bin" "$here/local.toml" >/dev/null 2>&1; then
    printf '%s\n' "FAIL: local.toml ran under -np 2 but should fail closed (local requires 1 rank)" >&2
    exit 1
fi
echo "OK fail-closed: split.toml rejected serially, local.toml rejected under mpirun -np 2"

printf '%s\n%s\n' "$local" "$mpi" | grep -E '^(LOCAL|MPI side|PASS)'

LOCAL="$local" MPI="$mpi" CONFIG="$here/config.toml" python3 - <<'PY'
import os, re, struct, tomllib

TOL = 5e-14
local = os.environ["LOCAL"]
mpi = os.environ["MPI"]
config = tomllib.load(open(os.environ["CONFIG"], "rb"))

def recurrence(config):
    def state(side):
        p = config[side]["oscillator"]
        return [p["x0"], p["v0"], p["peer_x0"], p]
    a, b = state("a"), state("b")
    result = []
    for _ in range(config["case"]["steps"]):
        def tick(q):
            x, v, peer, p = q
            accel = (-p["stiffness"] * x - p["damping"] * v
                     - p["coupling_stiffness"] * (x - peer)) / p["mass"]
            v += accel * p["dt"]
            return [x + v * p["dt"], v, peer, p]
        a, b = tick(a), tick(b)
        # Setup has already exchanged each side's initial position. The
        # iteration pump then imports the other side's fresh export.
        a[2], b[2] = b[0], a[0]
        result.append((a[0], a[1], b[0], b[1]))
    return result

def matches(pattern, text):
    return [m for line in text.splitlines() if (m := re.fullmatch(pattern, line))]

def check_trace(name, rows, reference):
    if len(rows) != len(reference):
        raise SystemExit(f"{name} trace incomplete: got {len(rows)} steps, expected {len(reference)}")
    error = max(abs(x-y) for row, ref in zip(rows, reference) for x, y in zip(row, ref))
    if error > TOL:
        raise SystemExit(f"{name} trajectory mismatch {error:.3e} exceeds {TOL:.1e}; inspect topology mode, rank order, handshake, and export-before-tick")
    return error

reference = recurrence(config)
local_rows = [tuple(float(m.group(i)) for i in range(2, 6))
              for m in matches(r"LOCAL_TRACE step=(\d+) a=([^,]+),([^ ]+) b=([^,]+),([^ ]+)", local)]

mpi_parts = {}
for m in matches(r"MPI_TRACE side=([ab]) step=(\d+) local=([^,]+),([^ ]+)", mpi):
    mpi_parts.setdefault(m.group(1), []).append((int(m.group(2)), float(m.group(3)), float(m.group(4))))
if set(mpi_parts) != {"a", "b"}:
    raise SystemExit("SPMD split output incomplete: expected trace lines from both roles")
steps = len(reference)
for side, rows in mpi_parts.items():
    if [row[0] for row in rows] != list(range(1, steps + 1)):
        raise SystemExit(f"SPMD split {side} trace has missing, duplicate, or out-of-order steps")
mpi_rows = [(a[1], a[2], b[1], b[2]) for a, b in zip(mpi_parts["a"], mpi_parts["b"])]

local_error = check_trace("Local", local_rows, reference)
mpi_error = check_trace("Split", mpi_rows, reference)
parity_error = check_trace("Split-vs-Local", mpi_rows, local_rows)
bits = lambda row: tuple(struct.unpack("Q", struct.pack("d", x))[0] for x in row)
fingerprints_match = bits(mpi_rows[-1]) == bits(local_rows[-1])
print(f"SPMD full trajectory: local_vs_recurrence={local_error:.3e} split_vs_recurrence={mpi_error:.3e} split_vs_local={parity_error:.3e}")
print(f"SPMD final fingerprint_match={fingerprints_match}")
print("PASS single-binary local and role-split full trajectories agree with the independent recurrence")
PY
