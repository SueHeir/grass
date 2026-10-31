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

Resources are keyed by their class, the way `Res<T>` is in Rust. A system parameter annotated `temp: Temperature` receives the `Temperature` resource:

```python
@dataclass
class Temperature:
    T: np.ndarray

@dataclass
class RodParams:
    r: float

def diffuse(temp: Temperature, p: RodParams) -> None:
    temp.T[1:-1] += p.r * (temp.T[2:] - 2 * temp.T[1:-1] + temp.T[:-2])

app = grass.App(config="[run]\nsteps = 200\n", output_dir="out")   # same TOML a Rust code reads
app.add_plugin(grass.plugins.Run())          # Rust plugins: Run, Checkpoint, Provenance
app.insert(Temperature(np.zeros(21)))
app.insert(RodParams(r=0.4))
app.add_system(diffuse, "diffuse")
app.run()
app[Temperature].T                           # typed as Temperature for pyright / mypy
```

* **Type checking.** The stubs in `python/typings` let pyright and mypy check field access inside systems and every `app[SomeClass]`. A resource no system can find is reported before step 1, naming the system and the class. Any class can be a resource type, so `app.insert(np.random.default_rng(7))` is received by `rng: np.random.Generator`.
* **Special parameters.** `sim: grass.Sim` gives `step`, `time` (settable), `stop()`, and `tick(Child, n)` in a parent. Unannotated parameters fall back to string keys (`app["name"] = ...`), and `res` gives the whole resource table. Both are an opt-in escape hatch.
* **Schedule.** Use `add_system(f, phase, label=, before=, after=, run_if=)`, `add_setup_system`, and `remove_system(f_or_label)`. `app.schedule()` lists the real execution order, including Rust plugin systems and their namespaces. Run conditions resolve resources the same way systems do.
* **Plugins.** A Python plugin is any object with `build(app)`. An optional `requires = ["OtherPlugin"]` is checked, and each plugin type can be added once. Rust plugins come from `grass.plugins`.
* **Checkpoints.** `app.checkpoint(Population, np.random.Generator)` pickles those resources into GRASS's checkpoint files, and `[checkpoint] restart = "..."` restores them. A restart is bit-identical.
* **Several codes.** `class Hot(grass.AppView): pass` makes a typed child marker, and `parent.add_app(Hot, hot_app)` moves the App under the parent. Parent systems annotated `hot: Hot` see `hot[Slab]` as a `Slab`. `parent.tick(Cold, 4)` subcycles declaratively, and `sim.tick(Cold, 4)` lets a parent system drive the children itself, which is what an implicit coupling loop needs. The parent ends when any child ends.
* **Errors.** An exception in a system stops the run and is re-raised from `run()` with a note naming the GRASS system.

`python/examples/app_walkthrough.py` shows all of this in four parts and type-checks clean under pyright. The last part runs two heat codes as child Apps under one parent, with the cold one subcycled 4x. Each time window is converged implicitly with IQN-ILS across non-matching interface meshes, taking 14 iterations in the first window and 4 by the last. Aitken takes 13 and 10.

### Limits of the preview

* Python systems run under the GIL, one at a time. That suits Python codes and drivers; heavy numerics should stay in NumPy or in Rust plugins.
* Rust plugins that operate on typed Rust resources (preCICE, mapped ports, DumpPlugin) are not yet exposed. Exposing one is a small wrapper per plugin.
* A Python App can't yet have a Rust solver App as a child. That needs a Rust solver compiled into, or loaded by, the module.
