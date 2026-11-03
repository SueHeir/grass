"""Write a synthetic B' table for micarus (cases/bprime.dat).

A real B' table comes from an equilibrium surface-chemistry solver run for
the ablator/air pair. This one is a smooth made-up surface with the right
shape: a diffusion-limited oxidation plateau that blowing suppresses, a
sublimation rise above ~3500 K that falls with pressure, and a wall
enthalpy that includes a dissociation hump. Plausible, not validated.

    python3 tools/make_bprime.py > cases/bprime.dat
"""
import math

P = [1.0e3, 1.0e4, 1.0e5]                         # Pa
BG = [0.0, 0.05, 0.1, 0.2, 0.4, 0.8, 1.6]           # B'g
T = [300.0 + 100.0 * i for i in range(43)]          # 300 .. 4500 K


def bc(t, bg, p):
    ox = 0.17 * 0.5 * (1 + math.tanh((t - 1500.0) / 200.0)) / (1 + 0.3 * bg)
    sub = 0.4 * math.exp((t - 3800.0) / 180.0) * math.sqrt(1.0e5 / p)
    return ox + min(sub, 6.0)


def hw(t, bg, p):
    diss = 2.0e6 * 0.5 * (1 + math.tanh((t - 3200.0) / 400.0)) * (1.0e5 / p) ** 0.05
    return 1200.0 * (t - 298.15) + diss - 1.0e6 * bg / (1 + bg)


print("# micarus B' table (synthetic): np nbg nt, then p [Pa], B'g, T [K],")
print("# then for each p, each B'g, each T:  B'c  h_w [J/kg]")
print(len(P), len(BG), len(T))
print(" ".join(f"{p:.6e}" for p in P))
print(" ".join(f"{g:.6e}" for g in BG))
print(" ".join(f"{t:.1f}" for t in T))
for p in P:
    for g in BG:
        for t in T:
            print(f"{bc(t, g, p):.10e} {hw(t, g, p):.10e}")
