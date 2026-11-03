"""Coupling study for the legacy code under GRASS.

Runs, from this directory (needs `make -C fortran` and `cargo build --release`):

1. the stand-alone program on the classic one-way table (micarus applies the
   blowing correction itself, from its own local blowing rate),
2. the GRASS-coupled entry (`entry3d`), explicit and implicit, at a 0.5 s
   and a 2 s coupling window,
3. the implicit 0.5 s case on 1, 2 and 3 micarus ranks, to check the code's
   own decomposition under GRASS,

and writes study_output.txt and coupling_study.png.
"""
import os
import re
import subprocess
import time

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

MPIRUN = ["mpirun", "--allow-run-as-root", "--oversubscribe"]
HERE = os.path.dirname(os.path.abspath(__file__))
os.chdir(HERE)


def run(cmd, cwd=HERE):
    t0 = time.time()
    out = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=True).stdout
    return out, time.time() - t0


def entry(scheme, window, tps_ranks):
    name = f"{scheme}_w{window}_r{tps_ranks}"
    src = open("entry3d.toml").read()
    src = re.sub(r'scheme = "\w+"', f'scheme = "{scheme}"', src)
    src = re.sub(r"window = [\d.]+", f"window = {window}", src)
    src = re.sub(r'(name = "tps"\nranks = )\d+', rf"\g<1>{tps_ranks}", src)
    src = re.sub(r'dir = "[^"]*"', f'dir = "out/study/{name}"', src)
    src = re.sub(r'case_name = "\w+"', 'case_name = "entry3d"', src)
    path = f"out/study/{name}.toml"
    os.makedirs("out/study", exist_ok=True)
    open(path, "w").write(src)
    out, wall = run(MPIRUN + ["-np", str(1 + tps_ranks), "target/release/entry3d", path])
    fp = re.search(r"fingerprint (\w+)", out).group(1)
    it = re.search(r"\(([\d.]+) per window, max (\d+)\)", out)
    rec = float(re.search(r"stagnation recession ([\d.]+) mm", out).group(1))
    return dict(name=name, dir=f"out/study/{name}", fp=fp, wall=wall, rec=rec,
                iters=float(it.group(1)) if it else 1.0, out=out)


def hist(path):
    rows = [l.split() for l in open(path) if not l.startswith("#")]
    cols = list(zip(*[[float(x) for x in r] for r in rows]))
    return dict(t=cols[0], tw=cols[3], mg=cols[4], mc=cols[5], rec=cols[6])


def flow_hist(path):
    lines = open(path).read().split("\n")
    head = lines[0].split(",")
    rows = [list(map(float, l.split(","))) for l in lines[1:] if l]
    return {h: [r[i] for r in rows] for i, h in enumerate(head)}


report = []
_, wall = run(MPIRUN + ["-np", "2", "../fortran/micarus", "oneway.nml"], cwd=os.path.join(HERE, "cases"))
one = hist("cases/out/oneway_hist.dat")
report.append(f"one-way table, stand-alone micarus on 2 ranks: stagnation recession "
              f"{one['rec'][-1] * 1e3:.3f} mm, peak Tw {max(one['tw']):.0f} K, wall {wall:.1f} s")

runs = {}
for scheme in ["explicit", "implicit"]:
    for w in [0.5, 2.0]:
        r = entry(scheme, w, 2)
        runs[(scheme, w)] = r
        report.append(f"{scheme:8s} window {w:3.1f} s, micarus on 2 ranks: stagnation recession "
                      f"{r['rec']:.3f} mm, {r['iters']:.2f} iterations/window, wall {r['wall']:.1f} s")

report.append("")
report.append("implicit, 0.5 s window, by micarus rank count (fingerprint of every face's final state):")
for n in [1, 2, 3]:
    r = runs[("implicit", 0.5)] if n == 2 else entry("implicit", 0.5, n)
    report.append(f"  {n} rank(s): recession {r['rec']:.3f} mm, fingerprint {r['fp']}, wall {r['wall']:.1f} s")

ref = runs[("implicit", 0.5)]
open("study_output.txt", "w").write("\n".join(report) + "\n")
print("\n".join(report))

# ---- plot ----
fig, ax = plt.subplots(1, 3, figsize=(15, 4.2))
ax[0].plot(one["t"], one["tw"], color="0.55", lw=2.2, label="one-way table (stand-alone)")
ax[2].plot(one["t"], [r * 1e3 for r in one["rec"]], color="0.55", lw=2.2, label="one-way table (stand-alone)")
styles = {("implicit", 0.5): ("#1f6fb4", "-"), ("explicit", 0.5): ("#e07b00", "--"),
          ("implicit", 2.0): ("#1f6fb4", ":"), ("explicit", 2.0): ("#c0392b", "-.")}
for key, (colr, ls) in styles.items():
    r = runs[key]
    h = hist(f"{r['dir']}/entry3d_hist.dat")
    f = flow_hist(f"{r['dir']}/flow_history.csv")
    lab = f"{key[0]}, {key[1]} s window"
    ax[0].plot(h["t"], h["tw"], color=colr, ls=ls, lw=1.4, label=lab)
    ax[1].plot(f["time"], f["blowing_factor_0"], color=colr, ls=ls, lw=1.4, label=lab)
    ax[2].plot(h["t"], [x * 1e3 for x in h["rec"]], color=colr, ls=ls, lw=1.4, label=lab)
ax[0].set(title="stagnation wall temperature", xlabel="time [s]", ylabel="T_w [K]")
ax[1].set(title="stagnation blowing factor ch/ch0 (flow code)", xlabel="time [s]", ylabel="ch / ch0", ylim=(0, 1.05))
ax[2].set(title="stagnation recession", xlabel="time [s]", ylabel="recession [mm]")
for a in ax:
    a.grid(alpha=0.3)
ax[2].legend(fontsize=8, loc="upper left")
fig.suptitle("micarus (legacy MPI Fortran) under GRASS: one-way table vs. two-way coupling", fontsize=11)
fig.tight_layout()
fig.savefig("coupling_study.png", dpi=130)
