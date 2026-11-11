# mapping_tour

`grass_map` on a heat-shield-shaped problem: a fluid grid above a
non-matching, differently triangulated shield surface, plus debris particles
in the flow. It exercises the three mapping patterns a coupled entry
simulation needs.

| Step | Map | Check |
| --- | --- | --- |
| Wall temperature, fluid cells to shield vertices | consistent trace (volume to surface) | linear field reproduced at interior vertices to `1e-9` K; no unmapped vertices |
| Wall heat, fluid wall faces to shield vertices | conservative (surface to non-matching surface) | total heat conserved to `1e-12` relative |
| Debris velocity and drag reaction | adjoint pair (particles and grid) | exchanged power and total force balance to `1e-12` relative |

The binary asserts every check and prints a PASS table.

## Run

```bash
cargo run --example mapping_tour
```
