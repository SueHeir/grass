"""Explicit vs implicit coupling as the coupling window grows.

Runs ./target/release/entry for each scheme and window (the ablator always
steps at 0.1 s, so only the coupling changes) and compares stagnation-point
results against an implicit run with a 0.1 s window.

    cargo build --release && python3 sweep.py
"""
import csv, os, re, subprocess

BASE = open("entry.toml").read()
WINDOWS = [0.1, 0.5, 1.0, 2.0, 4.0]


def run(scheme, window, accel="iqn"):
    out = f"out/sweep/{scheme}_{accel}_{window}"
    cfg = BASE
    cfg = re.sub(r'^scheme = .*$', f'scheme = "{scheme}"', cfg, flags=re.M)
    cfg = re.sub(r'^window = .*$', f'window = {window}', cfg, flags=re.M)
    cfg = re.sub(r'^tps_substeps = .*$', f'tps_substeps = {round(window / 0.1)}', cfg, flags=re.M)
    cfg = re.sub(r'^acceleration = .*$', f'acceleration = "{accel}"', cfg, flags=re.M)
    cfg = re.sub(r'^dir = .*$', f'dir = "{out}"', cfg, flags=re.M)
    os.makedirs(out, exist_ok=True)
    path = f"{out}/entry.toml"
    open(path, "w").write(cfg)
    text = subprocess.run(["./target/release/entry", path], capture_output=True, text=True, check=True).stdout
    rows = list(csv.DictReader(open(f"{out}/history.csv")))
    it = re.search(r"coupling iterations \(([\d.]+) per window", text)
    return {
        "t": [float(r["time"]) for r in rows],
        "tw": [float(r["t_wall_0"]) for r in rows],
        "bf": [float(r["blowing_factor_0"]) for r in rows],
        "mdot": [float(r["mdot_0"]) for r in rows],
        "rec": float(rows[-1]["recession_0"]) * 1e3,
        "peak_tw": max(float(r["t_wall_0"]) for r in rows),
        "iters": float(it.group(1)) if it else 1.0,
    }


ref = run("implicit", 0.1)
print(f"reference (implicit, 0.1 s window): recession {ref['rec']:.4f} mm, peak Tw {ref['peak_tw']:.1f} K\n")
print(f"{'window':>7} {'scheme':>9} {'recession err':>14} {'peak Tw err':>12} {'iters/window':>13}")
results = {}
for w in WINDOWS:
    for scheme in ("explicit", "implicit"):
        r = run(scheme, w)
        results[(scheme, w)] = r
        print(f"{w:7.1f} {scheme:>9} {100 * (r['rec'] - ref['rec']) / ref['rec']:13.2f}% "
              f"{r['peak_tw'] - ref['peak_tw']:10.1f} K {r['iters']:13.2f}")

print("\nacceleration at a 1 s window:")
for accel in ("picard", "aitken", "iqn"):
    r = run("implicit", 1.0, accel)
    print(f"  {accel:>7}: {r['iters']:.2f} iterations per window")

try:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
except ImportError:
    raise SystemExit

fig, ax = plt.subplots(1, 2, figsize=(11, 4))
ax[0].plot(ref["t"], ref["bf"], "k-", lw=2, label="reference (implicit, 0.1 s)")
for scheme, style in (("explicit", "--"), ("implicit", "-")):
    r = results[(scheme, 2.0)]
    ax[0].plot(r["t"], r["bf"], style, marker="o", ms=3, label=f"{scheme}, 2 s window")
ax[0].set_xlabel("time after entry interface [s]")
ax[0].set_ylabel("blowing reduction ch/ch0, stagnation point")
ax[0].legend()
ax[0].set_xlim(0, 80)

for scheme, m in (("explicit", "s"), ("implicit", "o")):
    ws = WINDOWS[1:]  # the 0.1 s implicit run is the reference itself
    err = [abs(results[(scheme, w)]["rec"] - ref["rec"]) / ref["rec"] * 100 for w in ws]
    ax[1].loglog(ws, err, marker=m, label=scheme)
ax[1].set_xlabel("coupling window [s]")
ax[1].set_ylabel("stagnation recession error [%]")
ax[1].legend()
ax[1].grid(True, which="both", alpha=0.3)
fig.suptitle("Fortran ablator + C aeroheating, coupled by GRASS")
fig.tight_layout()
fig.savefig("coupling_sweep.png", dpi=120)
print("\nwrote coupling_sweep.png")
