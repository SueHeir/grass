"""Strong and weak scaling of the coupled entry over MPI, on this machine.

The flow code is cheap and serial (one rank); the work is the Fortran
ablator, whose stations are split across the tps ranks. Every run must give
the same final-surface fingerprint as the in-process `entry` binary.

    cargo build --release --features mpi && python3 scaling.py
"""
import os, re, subprocess

ENV = dict(os.environ, OMPI_MCA_btl="self,vader", OMPI_ALLOW_RUN_AS_ROOT="1",
           OMPI_ALLOW_RUN_AS_ROOT_CONFIRM="1")
CORES = os.cpu_count()
BASE = open("entry_mpi.toml").read()


def config(stations, tps_ranks, path):
    angles = ", ".join(f"{60.0 * i / (stations - 1):.6f}" for i in range(stations))
    cfg = re.sub(r'(name = "tps"\nranks = )\d+', rf"\g<1>{tps_ranks}", BASE)
    cfg = re.sub(r"^stations_deg = .*$", f"stations_deg = [{angles}]", cfg, flags=re.M)
    cfg = re.sub(r"^stations = .*$", f"stations = {stations}", cfg, flags=re.M)
    cfg = re.sub(r"^cells = .*$", "cells = 100", cfg, flags=re.M)
    open(path, "w").write(cfg)


def grab(text, pattern):
    m = re.search(pattern, text)
    return m.group(1) if m else None


def run(cmd):
    out = subprocess.run(cmd, capture_output=True, text=True, env=ENV, check=True).stdout
    return (float(grab(out, r"stepping ([\d.]+) s") or "nan"),
            float(grab(out, r"total wall time ([\d.]+) s") or grab(out, r"wall time ([\d.]+) s")),
            grab(out, r"fingerprint (\w+)"))


os.makedirs("out/scaling", exist_ok=True)
print(f"{CORES} cores; flow takes 1 rank, tps takes the rest\n")

N = 1024
config(N, 1, "out/scaling/serial.toml")
# the in-process binary reads [flow.*]/[tps.*] the same way and ignores [topology]
ser = subprocess.run(["./target/release/entry", "out/scaling/serial.toml"], capture_output=True,
                     text=True, env=ENV, check=True).stdout
ref = grab(ser, r"fingerprint (\w+)")
ser_wall = float(grab(ser, r"wall time ([\d.]+) s"))
print(f"strong scaling, {N} stations x 100 cells")
print(f"  in-process (entry)          wall {ser_wall:6.2f} s   fingerprint {ref}")
base = None
for r in range(1, CORES):
    config(N, r, f"out/scaling/strong_{r}.toml")
    step, wall, fp = run(["mpirun", "--oversubscribe", "-np", str(r + 1), "./target/release/entry_mpi",
                          f"out/scaling/strong_{r}.toml"])
    base = base or wall
    ok = "identical" if fp == ref else f"DIFFERENT ({fp})"
    print(f"  mpirun -np {r + 1} (tps {r} rank{'s' if r > 1 else ' '})   wall {wall:6.2f} s   "
          f"speedup {base / wall:4.2f}   efficiency {100 * base / wall / r:5.1f}%   {ok}")

print(f"\nweak scaling, 512 stations per tps rank")
base = None
for r in range(1, CORES):
    config(512 * r, r, f"out/scaling/weak_{r}.toml")
    step, wall, fp = run(["mpirun", "--oversubscribe", "-np", str(r + 1), "./target/release/entry_mpi",
                          f"out/scaling/weak_{r}.toml"])
    base = base or wall
    print(f"  tps {r} rank{'s' if r > 1 else ' '}, {512 * r:5d} stations   wall {wall:6.2f} s   "
          f"efficiency {100 * base / wall:5.1f}%")
