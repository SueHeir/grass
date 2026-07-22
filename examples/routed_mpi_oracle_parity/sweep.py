"""Run the declarative real-MPI routed-exchange campaign and render its gate."""
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).parent
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import Canvas, BLACK, GREEN, RED

with (HERE / "config.toml").open("rb") as stream:
    config = tomllib.load(stream)

results = []
for check in config["check"]:
    command = [
        "cargo", "test", "-p", "grass_multi", "--features", "mpi",
        "--test", check["test"], check["filter"], "--", "--exact", "--nocapture", "--test-threads=1",
    ]
    result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True)
    results.append((check["label"], result.returncode == 0, result))

canvas = Canvas(900, 250)
canvas.text(30, 20, "ROUTED MPI ORACLE PARITY", scale=3)
canvas.text(30, 58, "EXPECTED PASS VS REAL MPI MEASUREMENT", scale=2)
canvas.text(610, 78, "EXPECTED", scale=2)
canvas.text(750, 78, "MEASURED", scale=2)
for index, (label, passed, _) in enumerate(results):
    y = 100 + index * 32
    canvas.text(30, y, label, scale=2)
    canvas.text(630, y, "PASS", scale=2)
    canvas.rect(750, y, 860, y + 18, GREEN if passed else RED)
    canvas.text(765, y, "PASS" if passed else "FAIL", scale=2)
canvas.save(HERE / "plots" / "routed_mpi_oracle_parity.png")

for label, passed, _ in results:
    print(f"{label}: {'PASS' if passed else 'FAIL'}")
if not all(passed for _, passed, _ in results):
    for _, passed, result in results:
        if not passed:
            print(result.stdout)
            print(result.stderr, file=sys.stderr)
    raise SystemExit(1)
print("PASS: 4/4 real-MPI routed-exchange checks")
