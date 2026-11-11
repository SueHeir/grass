# grass_map

Move data between any two discretizations: meshes, surfaces, particles,
point clouds. One sparse operator per pair, applied forwards for intensive
fields and transposed for extensive ones, so constants are preserved and
totals are conserved.

GRASS knows nothing about particles or meshes. Solver libraries implement the
`Discretization` trait for their own types; the built-ins cover common shapes
and serve as reference implementations.

## The idea

Every discretization answers two questions about itself:

| Question | Trait method | Examples |
| --- | --- | --- |
| Where and how do I integrate? | `sample_points` | Gauss points of a cell, a particle weighted by its volume, a 3-point rule per triangle |
| What is my value at this point? | `evaluate` | enclosing cell (constant), eight surrounding centers (trilinear), nearest triangle (projection, barycentric), particles within a kernel radius |

Sampling one side at the other's points gives the operator

```text
W[j, i] = Σ_{q ∈ j} w_q φ_i(x_q) / Σ_{q ∈ j, supported} w_q
```

| Map | Built as | Guarantee (when the basis is a partition of unity) |
| --- | --- | --- |
| `consistent()` src → dst | samples at the **target**, `W = assemble(dst, src)` | constants preserved |
| `conservative()` src → dst | samples at the **source**, `C = assemble(src, dst)ᵀ` | totals preserved exactly |
| `AdjointPair::new(a, b)` | one `W`; `sample` uses `W`, `deposit` uses `Wᵀ` | exchanged power balances: `Σ f·(W u) = Σ (Wᵀ f)·u` |

The conservative map `a → b` is exactly the transpose of the consistent map
`b → a`, which is what keeps two-way exchanges (drag and reaction, wall
temperature and wall heat) energy-consistent.

## Field kinds are types

| Kind | Meaning | Mapped by |
| --- | --- | --- |
| `Intensive` | value at a location: temperature, velocity | `ConsistentMap`, `AdjointPair::sample` |
| `Density` | amount per unit measure: heat flux, mass density | the same; convert with `to_extensive(&measures)` |
| `Extensive` | amount owned by an entity: force, heat, mass | `ConservativeMap`, `AdjointPair::deposit` |

Handing a force to a sampling map, or a temperature to a conservative one, is
a compile error.

## Sampling: interpolate or average

| `Sampling` | Sample points of the sampler | Use for |
| --- | --- | --- |
| `Average` (default for cells, faces) | its quadrature (`sample_points`) | cell and face data; a lumped L2 projection that smooths and spreads loads |
| `Centroid` (default for surface vertices) | one point per entity, weight 1 | nodal data; plain interpolation, exact for fields the source represents exactly |

## Built-in discretizations

| Type | Entities | `evaluate` |
| --- | --- | --- |
| `PointCloud` | points, optional measures | `Nearest` (optionally capped by distance) or `Kernel { radius }` (normalized Wendland C2) |
| `UniformGrid` | cell centers of a box | `Constant` or `Trilinear` (cloud-in-cell, clamped at walls); 1 to 3 Gauss points per axis |
| `TriSurface` | vertices (P1) or faces | projection onto the nearest triangle within a tolerance |

`Method::NearestNeighbor` overrides any basis with nearest-centroid assignment.

## Diagnostics

Every map carries a `MapReport`: entities with no supported sample points
(`unmapped`), entities only partly covered (`partial`), and the largest
deviation of the basis from a partition of unity. A consistent map leaves
unmapped targets untouched; a conservative map reports the source amounts it
could not place.

## Example

```rust
use grass_map::*;

let fluid = UniformGrid::new([0.0; 3], [1.0; 3], [24, 24, 12]).with_basis(CellBasis::Trilinear);
let particles = PointCloud::new(vec![[0.3, 0.4, 0.2], [0.7, 0.1, 0.5]]);

let pair = AdjointPair::new(&particles, &fluid, Method::Native);
let u = Field::<Intensive>::zeros(fluid.len(), 3);
let mut u_at_p = Field::<Intensive>::zeros(particles.len(), 3);
pair.sample(&u, &mut u_at_p);                  // fluid velocity at particles

let drag = Field::<Extensive>::zeros(particles.len(), 3);
let mut reaction = Field::<Extensive>::zeros(fluid.len(), 3);
pair.deposit(&drag, &mut reaction);            // reaction on the fluid, same operator
```

See [`examples/mapping_tour`](../../examples/mapping_tour/README.md) for a
heat-shield-shaped run with asserted checks.

## Scope and limits (phase 1)

- Serial. Operators are built on one process; distributing them over ranks
  with `grass_multi`'s routed exchange is planned.
- Rebuild by calling the builder again when geometry moves.
- No radial basis functions, exact supermesh intersection or ray bundles yet.
- Trilinear grids clamp to constant within half a cell of the walls, so they
  are first order there.

## License

MIT OR Apache-2.0
