# Public API and extension points

Grass keeps two use cases separate: an **application author** assembles an
executable from existing libraries; a **library author** supplies a reusable
plugin, scheduling vocabulary, transport, or coupling contract. The crate
roots are the complete public API. The preludes are deliberately smaller,
additive convenience imports for the first use case.

## Application authors

Use the application preludes for ordinary wiring, then name the few contracts
that a particular application owns explicitly:

```rust,ignore
use grass_app::prelude::*;
use grass_io::prelude::*;
use grass_multi::prelude::*;
use grass_scheduler::prelude::*;

#[derive(Clone, Copy, Debug, ScheduleSet)]
enum Phase { Step }

let mut app = App::new();
app.add_plugins((SimClockPlugin, TermOutPlugin));
```

`grass_app::prelude` and `grass_scheduler::prelude` cover the App lifecycle,
resources, systems, and scheduling vocabulary. `grass_io::prelude` covers the
optional declarative configuration and observability plugins.
`grass_multi::prelude` covers parent-App sub-App registration, local ticks,
and typed exchange ports. `grass_mpi::prelude` covers installing either the
serial or MPI communication resource. With its `mpi_backend` feature it also
includes the ordinary MPI lifecycle calls needed for that wiring:
`init_app_color` for an MPMD split, `get_mpi_world`, and `finalize_mpi` after
the resource is dropped. Raw-world transport/bootstrap accessors remain root
imports because they are coupling-specific rather than normal app wiring.

No prelude is required, and none changes what is available at a crate root.
That is intentional compatibility policy: existing explicit imports and
existing glob-prelude imports continue to work. Prefer a direct root import
when it makes an important contract visible at its use site.

## Library authors

Implement reusable extension points through explicit root imports. They are
not placed in the newer convenience preludes because implementing one is a
library-level commitment rather than routine application wiring.

| Need | Explicit extension point |
|---|---|
| Add reusable App wiring | `grass_app::Plugin` or `PluginGroup` |
| Define schedule phases | `grass_scheduler::ScheduleSet` (or `#[derive(ScheduleSet)]`) |
| Add a system parameter | `grass_scheduler::SystemParam` |
| Provide a capability contract | `grass_app::CapabilityId` and `Plugin::{provides_capabilities, requires_capabilities}` |
| Supply configuration metadata | `grass_io::DescribedConfig` (or `#[derive(ConfigDescription)]`) |
| Register a sub-App adapter | `grass_multi::Physics` |
| Define a remote payload or transport | `grass_multi::{Wire, Transport}` |
| Supply communication backend | `grass_mpi::CommBackend` |

Keep configuration declarative: plugins describe defaults and schemas through
typed metadata or TOML; they do not run scripts that alter registration or
scheduling structure. A coupling package or parent application owns the seam
between independently useful solver libraries and should exchange a stable
interface-owned `Port<T>` contract when that exchange is reused across
implementations. A pair-specific coupling package may instead use direct
`MultiRes` access to both participants' private resources.

## Surface and migration policy

The public roots remain the stable, fully supported surface. Items exposed only
for proc-macro expansion live in documented `__private` modules and are marked
`#[doc(hidden)]`; downstream applications should not use them directly.

Preludes are additive convenience modules. New prelude entries may be added;
removing or renaming an existing root export or prelude export requires a
deprecation period and a migration note here and in the affected crate's
rustdoc. This avoids compatibility churn while leaving room to hide a truly
accidental implementation detail before it becomes a supported downstream
contract.
