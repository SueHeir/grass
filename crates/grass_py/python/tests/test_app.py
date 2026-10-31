"""grass.App from Python. Run with: PYTHONPATH=<dir with grass.abi3.so> python test_app.py"""

import os
import tempfile

import numpy as np

import grass


def test_phases_order_and_run_plugin():
    app = grass.App(config="[run]\nsteps = 5\n")
    app.add_plugin(grass.plugins.Run())
    app["log"] = []
    app.add_phases("first", "second")
    app.add_system(lambda log: log.append("b"), "second", label="b")
    app.add_system(lambda log: log.append("a"), "first", label="a")
    assert app.run() == 5 and app.done
    assert app["log"] == ["a", "b"] * 5
    assert [s for _, _, s in app.schedule()][:2] == ["a", "b"]


def test_before_after_run_if_remove_and_stop():
    app = grass.App()
    app["log"] = []

    def p(log):
        log.append("p")

    def c(log):
        log.append("c")

    def gone(log):
        log.append("gone")

    app.add_system(c, "step", after=p)
    app.add_system(p, "step")
    app.add_system(gone, "step")
    app.add_system(lambda log: log.append("mid"), "step", label="mid", after=p, before=c)
    app.add_system(lambda log: log.append("even"), "step", label="even",
                   after="c", run_if=lambda sim: sim.step % 2 == 0)
    app.remove_system(gone)

    def stop(sim, log):
        if log.count("p") == 3:
            sim.stop()

    app.add_system(stop, "end")
    assert app.run(steps=100) == 3
    assert app["log"] == ["p", "mid", "c", "even"] * 3  # no clock: step stays 0


def test_python_plugins_and_dependencies():
    class A:
        def build(self, app):
            app["a"] = 1

    class B:
        requires = ["A"]

        def build(self, app):
            app.add_system(lambda a, res: res.__setitem__("b", a + 1), label="b")

    app = grass.App()
    try:
        app.add_plugin(B())
        raise AssertionError("missing dependency accepted")
    except ValueError:
        pass
    app.add_plugin(A())
    app.add_plugin(B())
    try:
        app.add_plugin(A())
        raise AssertionError("duplicate plugin accepted")
    except ValueError:
        pass
    app.run(steps=1)
    assert app["b"] == 2


def test_errors_name_the_system():
    app = grass.App()
    app.add_system(lambda nope: None, label="needs_nope")
    try:
        app.run(steps=1)
        raise AssertionError
    except KeyError as e:
        assert "needs_nope" in str(e) and "nope" in str(e)
    app = grass.App()
    app["x"] = 0

    def boom(x):
        raise ZeroDivisionError("bad")

    app.add_system(boom)
    try:
        app.run(steps=3)
        raise AssertionError
    except ZeroDivisionError as e:
        assert any("boom" in n for n in e.__notes__)
    assert app.steps == 1 and app.done


def test_python_state_restarts_bit_identically():
    out = tempfile.mkdtemp()

    def build(steps, extra=""):
        app = grass.App(config=f"[run]\nsteps = {steps}\n[checkpoint]\nevery = 10\nkeep = 0\n{extra}",
                        output_dir=out)
        app.add_plugin(grass.plugins.Checkpoint())
        app.add_plugin(grass.plugins.Run())
        app["x"] = np.linspace(0.1, 0.4, 4)
        app["rng"] = np.random.default_rng(1)
        app.checkpoint("x", "rng")

        def step(x, rng, sim):
            x[:] = np.clip(3.97 * x * (1 - x) + 1e-3 * rng.random(4), 0, 1)
            sim.time += 0.5

        app.add_system(step)
        return app

    full = build(30)
    full.run()
    again = build(10, f'restart = "{out}/checkpoints/checkpoint_0000000020.grass"')
    again.run()
    assert np.array_equal(full["x"], again["x"])
    cold = build(10)
    cold.run()
    assert not np.array_equal(full["x"], cold["x"])
    head = grass.read_checkpoint(os.path.join(out, "checkpoints", "checkpoint_0000000020.grass"))
    assert head["step"] == 20 and head["time"] == 10.0


def test_children_tick_and_parent_sees_their_resources():
    def counter(n_end=None):
        app = grass.App()
        app["n"] = [0]

        def inc(n, sim):
            n[0] += 1
            if n_end and n[0] >= n_end:
                sim.stop()

        app.add_system(inc)
        return app

    fast, slow = counter(), counter()
    parent = grass.App()
    parent.add_app("fast", fast)
    parent.add_app("slow", slow)
    parent.tick("fast", 3)
    parent.tick("slow")
    parent["seen"] = []
    parent.add_system(lambda fast, slow, seen: seen.append((fast.n[0], slow.n[0])), "after_tick",
                      label="look")
    parent.run(steps=2)
    assert parent["seen"] == [(3, 1), (6, 2)]
    assert fast["n"] == [6]  # still readable through the moved child
    try:
        fast.run()
        raise AssertionError
    except RuntimeError:
        pass

    # A parent system can tick children itself; a child that stops ends the parent.
    a = counter(n_end=7)
    parent = grass.App()
    parent.add_app("a", a)

    def drive(sim, a):
        sim.tick("a", 2)

    parent.add_system(drive)
    assert parent.run(steps=100) == 4 and a["n"] == [7]


if __name__ == "__main__":
    for name, f in list(globals().items()):
        if name.startswith("test_"):
            f()
            print("ok", name)
