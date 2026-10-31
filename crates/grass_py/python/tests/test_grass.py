"""Run with: PYTHONPATH=<dir with grass.abi3.so> python -m pytest python/tests (or python test_grass.py)."""

import numpy as np

import grass


def test_constants_and_totals():
    coarse = grass.UniformGrid((0, 0, 0), (1, 1, 1), (4, 4, 1))
    fine = grass.UniformGrid((0, 0, 0), (1, 1, 1), (9, 7, 1))
    t = grass.Map(coarse, fine)(np.full(16, 300.0))
    assert np.allclose(t, 300.0)
    q = grass.Map(coarse, fine, kind="extensive")(np.arange(16.0))
    assert abs(q.sum() - 120.0) < 1e-9
    m = grass.Map(coarse, fine).matrix()
    assert m.shape == (63, 16)
    assert np.allclose(np.asarray(m.sum(axis=1)).ravel(), 1.0)


def test_vectors_and_unmapped_fill():
    grid = grass.UniformGrid((0, 0, 0), (1, 1, 1), (2, 2, 2))
    probes = grass.PointCloud([[0.5, 0.5, 0.5], [5.0, 0.5, 0.5]])
    m = grass.Map(grid, probes)
    v = m(np.tile([1.0, 2.0, 3.0], (8, 1)), fill=np.nan)
    assert v.shape == (2, 3)
    assert np.allclose(v[0], [1, 2, 3]) and np.isnan(v[1]).all()
    assert m.report["unmapped"] == [1]


def test_adjoint_pair_balances():
    surf = grass.TriSurface([[0, 0, 0], [1, 0, 0], [0, 1, 0], [1, 1, 0]], [[0, 1, 3], [0, 3, 2]], tolerance=0.1)
    pts = grass.PointCloud(np.random.default_rng(0).random((50, 3)) * [1, 1, 0])
    pair = grass.AdjointPair(pts, surf)
    force = np.ones(50)
    assert abs(pair.deposit(force).sum() - 50.0) < 1e-9
    assert np.allclose(pair.sample(np.full(4, 7.0)), 7.0)


def test_iqn_ils_converges_a_strong_coupling():
    it = grass.OuterIteration([0.0, 0.0], scheme="iqn-ils", tol=1e-12)
    while not it.done:
        x = it.input
        it.observe([-2 * x[0] - x[1] + 1, -x[0] - 2 * x[1] + 2])
    assert it.converged and it.iters <= 4
    assert np.allclose(it.input, [1 / 8, 5 / 8])


def test_waveform():
    w = grass.Waveform(1)
    w.start(0.0, [1.0])
    w.push(0.5, [2.0])
    w.push(1.0, [4.0])
    assert w(0.25) == [1.5]
    assert w(0.25, "constant") == [2.0]


if __name__ == "__main__":
    for name, f in list(globals().items()):
        if name.startswith("test_"):
            f()
            print("ok", name)
