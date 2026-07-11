#!/usr/bin/env python3
"""Run the policies, independently validate them, and render measured results."""
import math, pathlib, re, subprocess, sys, tomllib
import numpy as np
from scipy.linalg import expm

ROOT = pathlib.Path(__file__).resolve().parents[2]
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / "examples"))
from plot_png import BLACK, BLUE, GREEN, RED, Canvas

cfg = tomllib.loads((HERE / "config.toml").read_text())
case, physical = cfg["case"], cfg["a"]["oscillator"]

def run():
    return subprocess.run(["cargo", "run", "--quiet", "--example", "oscillator_coupling_schemes"], cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=True).stdout

out = run()
if out != run(): raise SystemExit("nondeterministic oscillator-coupling output")
pattern = re.compile(r"^(explicit CSS|implicit Picard|relaxed Picard|adaptive retry) residual=([0-9.eE+-]+) iterations=(\d+) rejected=(\d+) accepted_dt=([0-9.eE+-]+) energy_ratio=([0-9.eE+-]+) relative_error=([0-9.eE+-]+) state=([0-9.eE+,-]+)", re.M)
rows = pattern.findall(out)
if len(rows) != 4: raise SystemExit(out)

# Independent reference: SciPy's Pade/scaling-squaring matrix exponential
# advances y'=Ay. It does not invoke the Rust exact-mode helper.
k, kc, mass, t = physical["stiffness"], physical["coupling_stiffness"], physical["mass"], case["final_time"]
matrix = np.array([[0., 1., 0., 0.], [-(k+kc)/mass, 0., kc/mass, 0.], [0., 0., 0., 1.], [kc/mass, 0., -(k+kc)/mass, 0.]])
y_exact = expm(matrix * t) @ np.array([physical["x0"], physical["v0"], cfg["b"]["oscillator"]["x0"], cfg["b"]["oscillator"]["v0"]])
checked = []
for name, residual, iterations, rejected, dt, energy, printed_error, state in rows:
    y = np.array([float(x) for x in state.split(",")])
    external_error = np.linalg.norm(y-y_exact) / np.linalg.norm(y_exact)
    if not math.isclose(external_error, float(printed_error), rel_tol=2e-8, abs_tol=2e-8):
        raise SystemExit(f"{name}: Rust and independent SciPy errors disagree: {printed_error} vs {external_error}")
    checked.append((name, external_error, float(energy), int(iterations), int(rejected), float(dt)))
by_name = {r[0]: r for r in checked}
if not (by_name["implicit Picard"][1] < .25 and by_name["relaxed Picard"][1] < .25 and by_name["adaptive retry"][1] < .25): raise SystemExit("converged policies miss external phase-space accuracy budget")
if not by_name["explicit CSS"][1] > .5: raise SystemExit("CSS lag not exposed against external reference")
if not (by_name["relaxed Picard"][3] < by_name["implicit Picard"][3] and by_name["adaptive retry"][4] > 0): raise SystemExit("required relaxation or retry behavior absent")

HERE.joinpath("plots").mkdir(exist_ok=True)
c = Canvas(820, 460)
c.text(38, 28, "COUPLING POLICIES: EXTERNAL PHASE-SPACE ERROR", BLACK, 3)
c.line(90, 365, 780, 365, BLACK); c.line(90, 70, 90, 365, BLACK)
for value, label in [(0., "0.00"), (.25, "0.25 accuracy"), (.5, "0.50 CSS"), (.7, "0.70")]:
    y = 365-int(value/.7*260); c.line(90,y,780,y,GREEN if value in (.25,.5) else BLACK); c.text(38,y-4,label,GREEN if value in (.25,.5) else BLACK,1)
for i,(name,error,energy,iterations,rejected,dt) in enumerate(checked):
    x=135+i*160; h=int(error/.7*260); c.rect(x,365-h,x+55,365,RED); c.text(x,375,name.replace(" ","\n"),BLACK,1); c.text(x,55,f"{error:.3f}",RED,1); c.text(x,430,f"it={iterations}, reject={rejected}",BLUE,1)
c.text(100,410,"red = independently computed SciPy expm relative error; green lines are stated criteria", BLACK, 1)
c.save(HERE / "plots" / "coupling_schemes.png")
print(out, end="")
print("PASS: independent SciPy matrix-exponential validation and deterministic repeat")
