#!/usr/bin/env bash
# Run by CI after building both MPI-only examples.  This is deliberately a
# launcher/checker, not configuration: all physical inputs live in config.toml.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
local=$(cargo run --quiet --example oscillator_mpmd_local)
mpi=$(mpirun -np 1 target/debug/examples/oscillator_mpmd_a : -np 1 target/debug/examples/oscillator_mpmd_b)
printf '%s\n%s\n' "$local" "$mpi"

LOCAL="$local" MPI="$mpi" python3 - <<'PY'
import os, re, struct

local = os.environ["LOCAL"]
mpi = os.environ["MPI"]
line = next(line for line in local.splitlines() if line.startswith("LOCAL "))
nums = [float(x) for x in re.search(r"a=([^ ]+) b=([^ ]+)", line).group(1).split(",")
        + re.search(r"a=([^ ]+) b=([^ ]+)", line).group(2).split(",")]
sides = {}
for line in mpi.splitlines():
    match = re.search(r"MPI side=([ab]) local=([^,]+),([^ ]+) mirror=([^ ]+)", line)
    if match:
        sides[match.group(1)] = tuple(float(match.group(i)) for i in range(2, 5))
if set(sides) != {"a", "b"}:
    raise SystemExit("MPMD output incomplete: expected one result line from each binary")
actual = [sides["a"][0], sides["a"][1], sides["b"][0], sides["b"][1]]
error = max(abs(a-b) for a, b in zip(actual, nums))
bits = lambda x: struct.unpack("Q", struct.pack("d", x))[0]
fingerprints_match = [bits(x) for x in actual] == [bits(x) for x in nums]
print(f"MPMD comparison max_abs_error={error:.3e} fingerprint_match={fingerprints_match}")
if error > 5e-14:
    raise SystemExit("MPMD trajectory mismatch exceeds 5e-14; inspect rank order, handshake, and export-before-tick")
print("PASS MPMD trajectory agrees with LocalTransport reference")
PY
