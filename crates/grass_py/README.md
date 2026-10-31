# grass_py (preview, not published)

Python bindings for the parts of grass that are useful without writing Rust:

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
