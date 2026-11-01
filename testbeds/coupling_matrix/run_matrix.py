#!/usr/bin/env python3
"""Run the coupling matrix and check every case.

    cargo build --release && python3 run_matrix.py [--quick] [-j N]

For each case, the serial reference and the coupled runs (roles as threads,
then one or more MPI rank layouts) must agree:

* explicit schemes: bit-identical to the reference;
* placements: roles as threads (CoupledRunner), as sub-Apps of one parent
  App (ImplicitCoupling), and as MPI ranks in several layouts;
* implicit schemes: within 1e-9 of the exact per-window fixed point, and
  bit-identical (values and iteration counts) across every placement;
* failure cases: every placement exits nonzero, and none hangs.
"""
import argparse, itertools, os, random, struct, subprocess, sys, tempfile, time
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.join(HERE, "target", "release", "coupling_matrix")
ENV = dict(os.environ, OMPI_ALLOW_RUN_AS_ROOT="1", OMPI_ALLOW_RUN_AS_ROOT_CONFIRM="1",
           OMPI_MCA_btl="self,vader", RUST_BACKTRACE="0")
TIMEOUT = 120


def toml_case(c, ranks):
    lines = ["[case]"]
    for k, v in c.items():
        if isinstance(v, str):
            lines.append(f'{k} = "{v}"')
        else:
            lines.append(f"{k} = {v!r}".replace("'", '"'))
    lines += ["", "[topology]", 'mode = "auto"']
    for r, n in enumerate(ranks):
        lines += ["[[topology.role]]", f'name = "r{r}"', f"ranks = {n}"]
    return "\n".join(lines) + "\n"


def run(cmd, path):
    t = time.time()
    try:
        p = subprocess.run(cmd + [path], capture_output=True, text=True, env=ENV, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        return None, "", "TIMEOUT", time.time() - t
    return p.returncode, p.stdout, p.stderr, time.time() - t


def parse(out):
    vals, iters, unconv = {}, None, 0
    for l in out.splitlines():
        if "VALUE " in l:
            i, b = l.split("VALUE ", 1)[1].split()[:2]
            vals[int(i)] = b
        elif "ITERS " in l:
            iters = l.split("ITERS ", 1)[1].strip()
        elif "UNCONVERGED " in l:
            unconv = int(l.split("UNCONVERGED ", 1)[1])
    return vals, iters, unconv


def f(b):
    return struct.unpack(">d", bytes.fromhex(b))[0]


def check(case_id, c, layouts):
    fail = c.get("fail_role") is not None
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "case.toml")
        notes = []
        # the reference (threads layout's toml is enough: topology is ignored)
        open(path, "w").write(toml_case(c, layouts[0]))
        rc, out, err, _ = run([BIN, "--reference"], path)
        if rc != 0:
            return False, f"reference failed: {err[-300:]}"
        ref, _, _ = parse(out)
        n = sum(c["components"])
        results = []
        placements = [("threads", layouts[0], [BIN])]
        if not fail:  # one process, one thread: a panic there is just a panic
            placements.append(("in-process", layouts[0], [BIN, "--inprocess"]))
        for lay in layouts[1:]:
            placements.append((f"mpi{lay}", lay, ["mpirun", "--oversubscribe", "-np", str(sum(lay)), BIN]))
        wall = 0.0
        for name, lay, cmd in placements:
            open(path, "w").write(toml_case(c, lay))
            rc, out, err, dt = run(cmd, path)
            wall += dt
            if fail:
                if rc is None:
                    return False, f"{name}: HUNG after injected failure"
                if rc == 0:
                    return False, f"{name}: exited 0 after injected failure"
                if "injected failure" not in err + out:
                    return False, f"{name}: failed, but not with the injected panic: {err[-300:]}"
                continue
            if rc != 0:
                return False, f"{name}: exit {rc}: " + " | ".join(err.strip().splitlines()[:12])
            vals, iters, unconv = parse(out)
            if unconv:
                return False, f"{name}: {unconv} windows hit the iteration cap (iters {iters})"
            if sorted(vals) != list(range(n)):
                return False, f"{name}: got components {sorted(vals)}, want 0..{n}"
            results.append((name, vals, iters))
        if fail:
            return True, f"all {len(placements)} placements failed cleanly ({wall:.1f}s)"
        base_name, base, base_it = results[0]
        for name, vals, iters in results[1:]:
            if vals != base or iters != base_it:
                diff = max(abs(f(vals[i]) - f(base[i])) for i in range(n))
                return False, f"{name} differs from {base_name}: max |diff| {diff:.3e}, iters {iters} vs {base_it}"
        err_ref = max(abs(f(base[i]) - f(ref[i])) for i in range(n))
        if c["scheme"].startswith("explicit"):
            if base != ref:
                return False, f"explicit run is not bit-identical to the reference (max |diff| {err_ref:.3e})"
            notes.append("bit-identical to reference")
        else:
            scale = max(1.0, max(abs(f(ref[i])) for i in range(n)))
            if err_ref > 1e-9 * scale:
                return False, f"implicit error {err_ref:.3e} vs exact fixed point (iters {base_it})"
            its = [int(x) for x in base_it.split(",")]
            notes.append(f"err {err_ref:.1e}, {sum(its)/len(its):.2f} it/window")
        notes.append(f"{len(placements)} placements agree ({wall:.1f}s)")
        return True, "; ".join(notes)


def layouts_for(components, rng, quick):
    """threads + a few MPI layouts, at most 8 ranks in total."""
    r = len(components)
    out = [[1] * r, [1] * r]
    if r <= 4:
        # one extra rank on each role in turn, and a more-ranks-than-components role
        out.append([2 if k == r - 1 else 1 for k in range(r)])
        out.append([min(3, components[k] + 1) if k == 0 else 1 for k in range(r)])
    if not quick:
        lay = [1 + rng.randrange(3) for _ in range(r)]
        while sum(lay) > 8:
            lay[lay.index(max(lay))] -= 1
        out.append(lay)
    seen, uniq = set(), []
    for k, l in enumerate(out):
        key = (k == 0, tuple(l))
        if key not in seen:
            seen.add(key)
            uniq.append(l)
    return uniq


def cases(quick):
    rng = random.Random(7)
    graphs = ["chain", "star", "ring", "complete"]
    schemes = ["explicit-jacobi", "explicit-gs", "implicit-jacobi", "implicit-gs"]
    out = []
    for roles in (2, 3, 4, 5):
        for graph in graphs:
            if roles == 2 and graph != "chain":
                continue  # all the same graph
            for scheme in schemes:
                for exchange in ("bridge", "routed"):
                    if quick and exchange == "routed" and scheme != "implicit-gs":
                        continue
                    comps = [1 + rng.randrange(5) for _ in range(roles)]
                    c = dict(graph=graph, components=comps, scheme=scheme, exchange=exchange,
                             acceleration="iqn" if rng.random() < 0.7 else "aitken",
                             substeps=[1 + rng.randrange(4) for _ in range(roles)],
                             windows=8, dt=0.25, coupling=0.6, seed=rng.randrange(1 << 30))
                    out.append((c, layouts_for(comps, rng, quick)))
    # stronger coupling, picard, and a single-component role on many ranks
    out.append((dict(graph="complete", components=[2, 1, 3], scheme="implicit-jacobi",
                     exchange="routed", acceleration="picard", substeps=[1, 1, 1],
                     windows=5, dt=0.2, coupling=0.3, seed=11), [[1, 1, 1], [1, 3, 1]]))
    out.append((dict(graph="ring", components=[4, 4, 4, 4], scheme="implicit-gs",
                     exchange="bridge", acceleration="iqn", substeps=[3, 1, 2, 1],
                     windows=6, dt=0.5, coupling=1.5, seed=5), [[1] * 4, [1] * 4, [2, 2, 2, 2]]))
    # failures: a role panics; the run must stop, not hang
    for scheme, exchange, role, lay in [("implicit-jacobi", "bridge", "r1", [1, 2, 1]),
                                       ("explicit-gs", "routed", "r2", [2, 1, 1]),
                                       ("implicit-gs", "routed", "r0", [1, 1, 2])]:
        out.append((dict(graph="ring", components=[3, 3, 3], scheme=scheme, exchange=exchange,
                         acceleration="iqn", substeps=[1, 2, 1], windows=6, dt=0.25,
                         coupling=0.6, seed=3, fail_role=role, fail_window=3), [[1, 1, 1], lay]))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true")
    ap.add_argument("-j", type=int, default=2)
    ap.add_argument("--only", type=int, default=None)
    a = ap.parse_args()
    all_cases = cases(a.quick)
    if a.only is not None:
        all_cases = [all_cases[a.only]]
    t0 = time.time()
    with ThreadPoolExecutor(a.j) as ex:
        futs = [ex.submit(check, k, c, l) for k, (c, l) in enumerate(all_cases)]
        failed = 0
        for k, ((c, l), fu) in enumerate(zip(all_cases, futs)):
            ok, msg = fu.result()
            failed += not ok
            tag = "ok  " if ok else "FAIL"
            desc = (f"{c['graph']:8} {len(c['components'])} roles {c['scheme']:15} "
                    f"{c['exchange']:6} {c['acceleration']:6}"
                    + (f" fail@{c['fail_role']}" if c.get("fail_role") else ""))
            print(f"[{k:3}] {tag} {desc} layouts {l[1:]}: {msg}", flush=True)
    print(f"\n{len(all_cases) - failed}/{len(all_cases)} cases passed in {time.time() - t0:.0f}s")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
