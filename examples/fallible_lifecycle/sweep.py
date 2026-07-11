"""Run independently measured fallible-lifecycle checks and render their matrix."""
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import Canvas, BLACK, GREEN, RED

CHECKS = [
    ("plugin build error", "app::tests::fallible_plugin_build_returns_app_error"),
    ("group short circuit", "app::tests::fallible_plugin_group_stops_after_failed_build"),
    ("nested plugin error", "app::tests::nested_fallible_plugin_registration_propagates_app_error"),
    ("setup short circuit", "app::tests::fallible_setup_stops_later_setup"),
    ("update blocked", "app::tests::fallible_setup_failure_prevents_update_execution"),
    ("cleanup after build", "app::tests::fallible_plugin_build_failure_runs_cleanup"),
    ("cleanup after setup", "app::tests::fallible_setup_failure_runs_cleanup"),
    ("cleanup after duplicate", "app::tests::duplicate_plugin_error_after_group_initialization_runs_cleanup"),
    ("cleanup after dependency", "app::tests::missing_dependency_after_group_initialization_runs_cleanup"),
    ("cleanup after prepare cap.", "app::tests::try_prepare_missing_capability_runs_cleanup"),
    ("cleanup after start cap.", "app::tests::try_start_missing_capability_runs_cleanup"),
    ("legacy compatibility", "app::tests::legacy_plugins_and_setup_systems_remain_compatible"),
]

results = []
for label, test_name in CHECKS:
    result = subprocess.run(
        ["cargo", "test", "-p", "grass_app", test_name, "--", "--exact"],
        cwd=ROOT,
        text=True,
        capture_output=True,
    )
    results.append((label, result.returncode == 0, result))

canvas = Canvas(800, 480)
canvas.text(30, 20, "FALLIBLE LIFECYCLE VALIDATION", scale=3)
canvas.text(30, 58, "INDEPENDENT MEASUREMENTS VS EXPECTED PASS", scale=2)
canvas.text(500, 78, "EXPECTED", scale=2)
canvas.text(650, 78, "MEASURED", scale=2)
for index, (check, passed, _) in enumerate(results):
    y = 95 + index * 30
    canvas.text(30, y, check, scale=2)
    canvas.text(515, y, "PASS", scale=2)
    canvas.rect(650, y, 760, y + 18, GREEN if passed else RED)
    canvas.text(665, y, "PASS" if passed else "FAIL", scale=2)
canvas.save(Path(__file__).parent / "plots" / "fallible_lifecycle_matrix.png")
for label, passed, _ in results:
    print(f"{label}: {'PASS' if passed else 'FAIL'}")
if not all(passed for _, passed, _ in results):
    for _, passed, result in results:
        if not passed:
            print(result.stdout)
            print(result.stderr, file=sys.stderr)
    raise SystemExit(1)
print("PASS")
