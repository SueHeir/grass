"""Run the fallible-lifecycle checks and render their pass/fail matrix."""
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import Canvas, BLACK, GREEN, RED

CHECKS = [
    "plugin build error",
    "group short circuit",
    "setup short circuit",
    "cleanup on error",
    "legacy compatibility",
]

result = subprocess.run(
    ["cargo", "test", "-p", "grass_app", "fallible_", "--", "--nocapture"],
    cwd=ROOT,
    text=True,
    capture_output=True,
)
passed = result.returncode == 0
canvas = Canvas(760, 260)
canvas.text(30, 20, "FALLIBLE LIFECYCLE VALIDATION", scale=3)
canvas.text(30, 58, "MEASURED TEST RESULT VS REQUIRED PASS", scale=2)
for index, check in enumerate(CHECKS):
    y = 95 + index * 30
    canvas.text(30, y, check, scale=2)
    canvas.rect(400, y, 610, y + 18, GREEN if passed else RED)
    canvas.text(625, y, "PASS" if passed else "FAIL", scale=2)
canvas.save(Path(__file__).parent / "plots" / "fallible_lifecycle_matrix.png")
print("PASS" if passed else "FAIL")
if not passed:
    print(result.stdout)
    print(result.stderr, file=sys.stderr)
    raise SystemExit(result.returncode)
