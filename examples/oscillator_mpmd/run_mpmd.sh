#!/usr/bin/env bash
# Run by CI after building both MPI-only examples.  This is deliberately a
# launcher/checker, not configuration: all physical inputs live in config.toml.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
local=$(cargo run --quiet --example oscillator_mpmd_local)
set +e
mpi=$(mpirun -np 1 target/debug/examples/oscillator_mpmd_a : -np 1 target/debug/examples/oscillator_mpmd_b 2>&1)
mpi_status=$?
set -e

# Some packaged Open MPI/PRRTE combinations crash in the launcher's MPMD
# colon/appfile parser (even for `/bin/true : /bin/true`).  Exit 139 is thus
# distinguished from an application failure and retried through one SPMD
# shell allocation that execs the intended binary on each world rank.  The
# resulting MPI_COMM_WORLD and the two application processes are identical
# from the binaries' perspective.
if [ "$mpi_status" -eq 139 ]; then
    mpi=$(mpirun -np 2 sh -c '
        rank=${OMPI_COMM_WORLD_RANK:-${PMI_RANK:-${PMIX_RANK:-}}}
        case "$rank" in
            0) exec target/debug/examples/oscillator_mpmd_a ;;
            1) exec target/debug/examples/oscillator_mpmd_b ;;
            *) echo "cannot determine MPI world rank for MPMD dispatch" >&2; exit 2 ;;
        esac
    ' 2>&1) || {
        printf '%s\n' "$mpi" >&2
        printf '%s\n' "MPMD launcher fallback failed before a complete trace." >&2
        exit 1
    }
elif [ "$mpi_status" -ne 0 ]; then
    printf '%s\n' "$mpi" >&2
    printf '%s\n' "MPMD launcher failed before a complete trace; verify MPI runtime/library compatibility, then rank order (-np 1 a : -np 1 b)." >&2
    exit 1
fi
printf '%s\n%s\n' "$local" "$mpi"

LOCAL="$local" MPI="$mpi" CONFIG="examples/oscillator_mpmd/config.toml" python3 - <<'PY'
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
        # Setup has already exchanged each side's initial position.  The
        # iteration pump then imports the other side's fresh export.
        a[2], b[2] = b[0], a[0]
        result.append((a[0], a[1], b[0], b[1]))
    return result

def fullmatch(pattern, text):
    out = []
    for line in text.splitlines():
        m = re.fullmatch(pattern, line)
        if m: out.append(m)
    return out

def check_trace(name, rows, reference):
    if len(rows) != len(reference):
        raise SystemExit(f"{name} trace incomplete: got {len(rows)} steps, expected {len(reference)}")
    error = max(abs(x-y) for row, ref in zip(rows, reference) for x,y in zip(row, ref))
    if error > TOL:
        raise SystemExit(f"{name} trajectory mismatch {error:.3e} exceeds {TOL:.1e}; inspect rank order, handshake, and export-before-tick")
    return error

reference = recurrence(config)
local_matches = fullmatch(r"LOCAL_TRACE step=(\d+) a=([^,]+),([^ ]+) b=([^,]+),([^ ]+)", local)
local_rows = [tuple(float(m.group(i)) for i in range(2, 6)) for m in local_matches]
mpi_parts = {}
for m in fullmatch(r"MPI_TRACE side=([ab]) step=(\d+) local=([^,]+),([^ ]+)", mpi):
    mpi_parts.setdefault(m.group(1), []).append((int(m.group(2)), float(m.group(3)), float(m.group(4))))
if set(mpi_parts) != {"a", "b"}:
    raise SystemExit("MPMD output incomplete: expected trace lines from both binaries")
steps = len(reference)
for side, rows in mpi_parts.items():
    if [row[0] for row in rows] != list(range(1, steps + 1)):
        raise SystemExit(f"MPMD {side} trace has missing, duplicate, or out-of-order steps")
mpi_rows = [(a[1], a[2], b[1], b[2]) for a, b in zip(mpi_parts["a"], mpi_parts["b"])]

local_error = check_trace("LocalTransport", local_rows, reference)
mpi_error = check_trace("MPMD", mpi_rows, reference)
parity_error = check_trace("MPMD-vs-LocalTransport", mpi_rows, local_rows)
bits = lambda row: tuple(struct.unpack("Q", struct.pack("d", x))[0] for x in row)
fingerprints_match = bits(mpi_rows[-1]) == bits(local_rows[-1])
print(f"MPMD full trajectory: local_vs_recurrence={local_error:.3e} mpi_vs_recurrence={mpi_error:.3e} mpi_vs_local={parity_error:.3e}")
print(f"MPMD final fingerprint_match={fingerprints_match}")
if not fingerprints_match:
    print("NOTE fingerprint policy: tolerated when all 40 values meet the 5e-14 numerical trajectory criterion; heterogeneous floating-point paths may differ in bits.")
print("PASS MPMD and LocalTransport full trajectories agree with the independent recurrence")
PY
