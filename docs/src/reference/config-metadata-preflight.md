# Configuration metadata preflight

The generated configuration work considered maintained Serde-compatible Rust
approaches before adding a Grass contract:

- [`schemars` 1.2.1](https://crates.io/crates/schemars) generates JSON Schema
  from Rust types. Its JSON-Schema target does not produce Grass's ordered TOML
  examples or stable Rust field-source locations.
- [`serde-reflection` 0.6.0](https://crates.io/crates/serde-reflection)
  extracts wire-format type representations. It does not retain Serde defaults,
  prose, or source locations needed for a practical field reference.
- [`serde_derive_internals` 0.29.1](https://crates.io/crates/serde_derive_internals)
  exposes Serde derive internals, but labels itself unstable and is not suitable
  for a public Grass configuration contract.

Grass therefore keeps Serde as the parser and adds a small declarative
`ConfigDescription` adjacent to each typed config. `DescribedConfig` ties that
description's section key to `Config::load_described`, and regression tests
compare every serialized typed default field with generated TOML. This supports
defaults, required status, enum choices, narrative comments, and source
locations without placing a discretization assumption in `grass_app`.
