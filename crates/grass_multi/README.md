# grass_multi

Cross-namespace coupling for [`grass_app`](../grass_app/). Register
several independent `App`s as named sub-Apps under one parent App; the
parent's schedule decides when each sub-App ticks and when cross-
namespace coupling systems run.

## What you actually use

| item | what it does |
|---|---|
| [`MultiAppExt::add_subapp`](src/multi.rs) | register a child `App` under a name |
| [`MultiAppExt::add_remote_subapp`](src/multi.rs) | register a remote-process mirror over a [`Transport`](src/transport.rs); fluent `.send_each_iter::<T>().recv_each_iter::<T>().with_resource::<U>()` |
| [`tick_subapp`](src/multi.rs) | system constructor — advance the named sub-App once per parent-iter |
| [`MultiRes<T, NS>`](src/typed_multi.rs) / [`MultiResMut<T, NS>`](src/typed_multi.rs) | typed cross-namespace SystemParams; use them in coupling systems exactly like `Res<T>` / `ResMut<T>` |
| [`Namespace`](src/multi.rs) trait + [`namespace!`](src/multi.rs) macro | compile-time markers for sub-App names |
| [`Wire`](src/wire.rs) | impl on resources that cross the wire (the MPI two-binary case) — defines `pack` / `unpack` bytes |
| [`MpiInterCommTransport`](src/transport.rs) (feature `mpi`) | point-to-point on `MPI_COMM_WORLD` for MPMD launches |
| [`OuterIterStopPlugin`](src/outer_iter.rs) | drop into the parent's schedule for fixed-iter termination |

## Example shape

```rust
use grass_app::prelude::*;
use grass_multi::{namespace, tick_subapp, MultiAppExt, MultiRes, MultiResMut, Namespace};
use grass_scheduler::prelude::*;

namespace!(pub A = "a");
namespace!(pub B = "b");

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Phase { Tick, Couple }

fn exchange(a: MultiRes<MyState, A>, mut b_other: MultiResMut<MyOther, B>) {
    b_other.x = a.x;
}

let mut parent = App::new();
parent.add_subapp(A::NAME, app_a);
parent.add_subapp(B::NAME, app_b);
parent.add_update_system(tick_subapp(A::NAME, 1), Phase::Tick);
parent.add_update_system(tick_subapp(B::NAME, 1), Phase::Tick);
parent.add_update_system(exchange, Phase::Couple);
parent.start();
```

## Also exposed (escape hatches)

These are public for power-user / interop reasons but most code never
reaches for them:

- [`Multi`](src/multi.rs) — the string-keyed SystemParam under
  `MultiRes` / `MultiResMut`. Use when the namespace is runtime data.
- [`Transport`](src/transport.rs) trait + [`LocalTransport`](src/transport.rs)
  — implement `Transport` for a custom wire (e.g. ZeroMQ, shared memory).
  `LocalTransport::pair()` is in-memory channels for tests.
- [`Physics`](src/physics.rs) trait + `AppPhysics` + `RemoteMirrorPhysics`
  — the wrappers behind `add_subapp` / `add_remote_subapp`. Implement
  `Physics` directly if you need a sub-App backend that's neither.
- [`snapshot_subapp_resource`](src/snapshot.rs) /
  [`restore_subapp_resource`](src/snapshot.rs) — system constructors for
  save / restore of a sub-App's `T` resource. The worked
  Picard / adaptive examples roll their own with `MultiResMut`; these
  helpers exist for cases where you want to keep the save state internal.

## See also

- [`examples/coupling/`](../../examples/coupling/) — five worked
  examples that build up coupling complexity one schedule at a time,
  ending with a two-binary MPI version. The README there walks through
  them as a story.
- [`grass_scheduler`](../grass_scheduler/) — the schedule / loop / branch
  primitives the parent App uses.
- [`grass_app`](../grass_app/) — the App / Plugin / Resource layer.
