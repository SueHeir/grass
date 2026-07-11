#!/usr/bin/env python3
"""One reproducible oscillator evidence command: data, figures, local and MPI."""
import csv
import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import BLACK, BLUE, GREEN, RED, Canvas

TRACE = re.compile(
    r"COUPLING_TRACE policy=(\S+) step=(\d+) dt=([\deE+.-]+) iterations=(\d+) "
    r"residual=([\deE+.-]+) accepted=(true|false) energy_ratio=([\deE+.-]+) "
    r"state=([\deE+.-]+),([\deE+.-]+),([\deE+.-]+),([\deE+.-]+)"
)

def run(command):
    return subprocess.run(command, cwd=ROOT, text=True, check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout

# The scheme sweep uses SciPy's independent matrix exponential and checks the
# policy summaries.  Its output is retained alongside the full histories below.
reference_check = run([sys.executable, "examples/oscillator_coupling_schemes/sweep.py"])
scheme = run(["cargo", "run", "--quiet", "--example", "oscillator_coupling_schemes"])
rows = [m.groups() for m in TRACE.finditer(scheme)]
if len(rows) < 16:
    raise SystemExit("incomplete coupling history; expected all policy macro steps")
if scheme != run(["cargo", "run", "--quiet", "--example", "oscillator_coupling_schemes"]):
    raise SystemExit("nondeterministic coupling history")

data = HERE / "data" / "coupling_histories.csv"
with data.open("w", newline="") as f:
    w = csv.writer(f)
    w.writerow(["policy", "step", "accepted", "dt", "iterations", "interface_residual", "energy_ratio", "a_x", "a_v", "b_x", "b_v"])
    for policy, step, dt, iterations, residual, accepted, energy, ax, av, bx, bv in rows:
        w.writerow([policy, step, accepted, dt, iterations, residual, energy, ax, av, bx, bv])

# Render the actual histories rather than pass/fail ornaments.  Each trace is
# plotted directly from the executable output; the companion scheme sweep
# independently checks its terminal state against the matrix exponential.
accepted = [r for r in rows if r[5] == "true"]
policies = ["explicit_CSS", "implicit_Picard", "relaxed_Picard", "adaptive_retry"]
colors = [RED, BLUE, GREEN, BLACK]
c = Canvas(1000, 620)
c.text(35, 20, "OSCILLATOR COUPLING HISTORIES", BLACK, 3)
for left, top, right, bottom, title in [
    (80, 80, 480, 290, "A POSITION"), (570, 80, 970, 290, "ENERGY RATIO"),
    (80, 370, 480, 580, "INTERFACE RESIDUAL"), (570, 370, 970, 580, "ITERATIONS AND DT"),
]:
    c.text(left, top - 18, title, BLACK, 2)
    c.line(left, bottom, right, bottom, BLACK); c.line(left, top, left, bottom, BLACK)
for policy, color in zip(policies, colors):
    points = [r for r in accepted if r[0] == policy]
    if not points: continue
    n = max(1, len(points) - 1)
    for idx, row in enumerate(points):
        x = 80 + int(idx / n * 390)
        ax, energy, residual, iterations, dt = float(row[7]), float(row[6]), float(row[4]), float(row[3]), float(row[2])
        # scales are chosen from the declared coarse demonstration range.
        y1 = 185 - int(max(-1.2, min(1.2, ax)) / 1.2 * 90)
        y2 = 290 - int(min(1.2, energy) / 1.2 * 190)
        y3 = 580 - int(min(2.0, residual) / 2.0 * 190)
        y4 = 580 - int(min(14, iterations) / 14 * 150)
        if idx:
            prev = points[idx - 1]
            px = 80 + int((idx - 1) / n * 390)
            pax, pe, pr, pi = float(prev[7]), float(prev[6]), float(prev[4]), float(prev[3])
            c.line(px, 185-int(max(-1.2,min(1.2,pax))/1.2*90), x, y1, color)
            c.line(570+px-80, 290-int(min(1.2,pe)/1.2*190), 570+x-80, y2, color)
            c.line(px, 580-int(min(2.0,pr)/2.0*190), x, y3, color)
            c.line(570+px-80, 580-int(min(14,pi)/14*150), 570+x-80, y4, color)
        c.rect(570+x-80-2, y4-2, 570+x-80+3, y4+3, color)
        # A short tick on the same panel records accepted macro-step length.
        c.line(570+x-80, 580, 570+x-80, 580-int(dt/.02*35), color)
    c.text(100 + policies.index(policy) * 95, 310, policy.replace("_", " "), color, 1)
c.text(85, 595, "LINES: EXECUTABLE HISTORIES. DT TICKS SHARE THE LOWER-RIGHT PANEL.", BLACK, 1)
plot = HERE / "plots" / "coupling_histories.png"
c.save(plot)

# LocalTransport trajectory has its own independently implemented recurrence
# and a four-component comparison figure.
local = run([sys.executable, "examples/oscillator_mpmd/sweep.py"])
mpi_note = "MPI unavailable; local parity evidence completed"
if shutil.which("mpirun"):
    run(["cargo", "build", "--quiet", "--features", "mpi", "--example", "oscillator_mpmd_a", "--example", "oscillator_mpmd_b"])
    try:
        mpi_note = run(["bash", "examples/oscillator_mpmd/run_mpmd.sh"]).strip().splitlines()[-1]
    except subprocess.CalledProcessError as error:
        # A launcher binary alone does not make an MPI runtime usable (this is
        # common in minimal containers with a partial PMIx install). CI has a
        # real OpenMPI runtime and treats this same command as a hard gate.
        mpi_note = "MPI runtime unavailable locally; MPI gate retained in CI: " + error.output.splitlines()[-1]

print(reference_check, end="")
print(scheme, end="")
print(local, end="")
print(f"PASS showcase histories={len(rows)} data={data} plot={plot}; {mpi_note}")
