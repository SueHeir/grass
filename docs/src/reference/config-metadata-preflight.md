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

Grass therefore keeps Serde as the parser and adds a small `ConfigDescription`
derive that reads its struct fields, Serde attributes, and documentation.
`DescribedConfig` ties that description's section key to `Config::load_described`,
and regression tests compare every serialized typed default field with generated
TOML. This supports defaults, required status, enum choices, narrative comments, and source
locations without placing a discretization assumption in `grass_app`.

The field reference distinguishes a Serde parser default from a starter-file
example. A Rust `Default` implementation is not evidence that Serde accepts an
omitted key: required fields are labelled **Required**, while their typed value
is still emitted so the generated TOML is a complete, parseable starting file.
The downstream contract test verifies both facts: omitting the key is rejected,
and the generated file is accepted.

Members marked `#[serde(skip)]` or `#[serde(skip_deserializing)]` are omitted
from the reference because they are not input keys. This prevents a generated
reference from advertising state that a `deny_unknown_fields` configuration
would reject (or that a permissive configuration would ignore).
