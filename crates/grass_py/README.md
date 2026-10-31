# grass_py (preview, not published)

Python bindings for grass. Two layers:

* **The framework** (`grass.App`): build a GRASS App in Python, with resources, systems in named phases, plugins, child Apps and coupling. See "Apps from Python" below.
* **The toolbox**, usable from any Python code:

  * **Mapping** (`grass_map`): `PointCloud`, `UniformGrid`, `TriSurface`, `Map(src, dst, kind=...)` and `AdjointPair`. Inputs are NumPy arrays; `Map.matrix()` returns a `scipy.sparse.csr_matrix`.
  * **Coupling** (`grass_multi`): `OuterIteration` (fixed, Aitken or IQN-ILS with window reuse) and `Waveform`, to couple Python solvers.
  * **Checkpoints** (`grass_io`): `read_checkpoint(path)` returns a verified checkpoint header as a dict.

```python
import numpy as np
import grass

coarse = grass.UniformGrid((0, 0, 0), (1, 1, 1), (4, 4, 1))
fine = grass.UniformGrid((0, 0, 0), (1, 1, 1), (9, 7, 1))
t_fine = grass.Map(coarse, fine)(np.full(16, 300.0))                       # constants preserved
q_fine = grass.Map(coarse, fine, kind="extensive")(np.arange(16.0))        # totals preserved

it = grass.OuterIteration(x0, scheme="iqn-ils", reuse=4)
while not it.done:
    it.observe(solve_both(it.input))
it.begin_window()
```

## Building

```sh
cargo build --release -p grass_py
cp target/release/libgrass.so grass.abi3.so      # .dylib on macOS, .dll -> .pyd on Windows
PYTHONPATH=. python crates/grass_py/python/tests/test_grass.py
```

The crate targets the stable ABI (`abi3-py39`), so one build works on Python 3.9 and later. Packaging with `maturin` would be the next step.

## Example

`python/examples/coupled_slabs.py` couples two Python heat solvers on non-matching interface meshes (8 x 8 cells against 3000 scattered triangles) with Dirichlet-Neumann coupling. Plain iteration diverges; Aitken and IQN-ILS converge every window, IQN-ILS in about half the iterations once it reuses earlier windows.

## Apps from Python

```python
app = grass.App(config="[run]\nsteps = 200\n", output_dir="out")   # same TOML a Rust code reads
app.add_plugin(grass.plugins.Run())          # Rust plugins: Run, Checkpoint, Provenance
app["T"] = np.zeros(21)                      # resources are Python objects, by name
app["params"] = {"r": 0.4}
app.add_phases("boundary", "diffuse")        # phases run in declaration order

def diffuse(T, params):                      # parameter names pick the resources
    T[1:-1] += params["r"] * (T[2:] - 2 * T[1:-1] + T[:-2])

app.add_system(diffuse, "diffuse")
app.run()
```

* **Systems** take resources by parameter name. Two names are special. `sim` gives `step`, `time` (settable), `stop()`, and `tick(child, n)` in a parent. `res` gives the App's whole resource table.
* **Schedule.** Use `add_system(f, phase, label=, before=, after=, run_if=)`, `add_setup_system`, and `remove_system(label_or_function)`. `app.schedule()` lists the order the scheduler will run, including Rust plugin systems and their namespaces. Within a phase, `before` and `after` are honoured by GRASS's topological sort.
* **Plugins.** A Python plugin is any object with `build(app)`. An optional `requires = ["OtherPlugin"]` is checked, and each plugin type can be added once. Rust plugins come from `grass.plugins`.
* **Checkpoints.** `app.checkpoint("x", "rng")` pickles those resources into GRASS's checkpoint files, and `[checkpoint] restart = "..."` restores them. A restart is bit-identical, including NumPy RNG state.
* **Several codes.** `parent.add_app("hot", hot_app)` moves an App under a parent. Parent systems with a parameter named `hot` see its resources. `parent.tick("cold", 4)` subcycles a child declaratively. `sim.tick("cold", 4)` lets a parent system drive the children itself, which is what an implicit coupling loop needs. The parent ends when any child ends.
* **Errors.** An exception in a system stops the run and is re-raised from `run()` with a note naming the GRASS system. A missing resource names both the system and the resource.

`python/examples/app_walkthrough.py` shows all of this in four parts. The last part runs two heat codes as child Apps of one parent, with the cold one subcycled 4x. Each time window is converged implicitly with IQN-ILS across non-matching interface meshes, taking 14 iterations in the first window and 4 by the last. Aitken takes 13 and 10.

### Limits of the preview

* Python systems run under the GIL, one at a time. That suits Python codes and drivers; heavy numerics should stay in NumPy or in Rust plugins.
* Rust plugins that operate on typed Rust resources (preCICE, mapped ports, DumpPlugin) are not yet exposed. Exposing one is a small wrapper per plugin.
* A Python App can't yet have a Rust solver App as a child. That needs a Rust solver compiled into, or loaded by, the module.
