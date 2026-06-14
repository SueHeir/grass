# grass_multi

Cross-namespace coupling for [`grass_app`](../grass_app/). Register
several independent `App`s as named sub-Apps under one parent App; the
parent's schedule decides when each sub-App ticks and when cross-
namespace coupling systems run.

There is no orchestrator type, no Strategy enum, no Coupler trait — the
parent App's own schedule is the orchestrator.

## What you actually use

| item | what it does |
|---|---|
| [`MultiAppExt::add_subapp`](src/multi.rs) | register a child `App` under a name |
| [`MultiAppExt::add_subapp_typed::<NS>`](src/multi.rs) | same, keyed by a [`Namespace`] marker (typo-safe) |
| [`MultiAppExt::add_remote_subapp`](src/multi.rs) | register a remote-process mirror over a [`Transport`](src/transport.rs); fluent `.send_at_setup::<T>().send_each_iter::<T>().recv_each_iter::<T>().with_resource::<U>()` |
| [`tick_subapp`](src/multi.rs) / [`tick_n_times::<NS>`](src/multi.rs) | system constructors — advance a sub-App `n` times per parent iter (string- or marker-keyed) |
| [`MultiRes<T, NS>`](src/typed_multi.rs) / [`MultiResMut<T, NS>`](src/typed_multi.rs) | typed cross-namespace SystemParams; use them in coupling systems exactly like `Res<T>` / `ResMut<T>` |
| [`Namespace`](src/multi.rs) trait — via `#[derive(Namespace)]` or the [`namespace!`](src/multi.rs) macro | compile-time markers for sub-App names |
| [`Wire`](src/wire.rs) | impl on resources that cross the wire (the MPI two-binary case) — defines `pack` / `unpack` bytes |
| [`MpiInterCommTransport`](src/transport.rs) (feature `mpi`) | point-to-point on `MPI_COMM_WORLD` for MPMD launches |
| [`OuterIterStopPlugin`](src/outer_iter.rs) | drop into the parent's schedule for fixed-iter termination |

## Example shape

```rust
use grass_app::prelude::*;
use grass_multi::{tick_n_times, MultiAppExt, MultiRes, MultiResMut, Namespace};
use grass_scheduler::prelude::*;

#[derive(Namespace)] pub struct A;
#[derive(Namespace)] pub struct B;

#[derive(Debug, Clone, Copy, ScheduleSet)]
enum Phase { Tick, Couple }

fn exchange(a: MultiRes<MyState, A>, mut b_other: MultiResMut<MyOther, B>) {
    b_other.x = a.x;
}

let mut parent = App::new();
parent.add_subapp_typed::<A>(app_a);
parent.add_subapp_typed::<B>(app_b);
parent.add_update_system(tick_n_times::<A>(1), Phase::Tick);
parent.add_update_system(tick_n_times::<B>(1), Phase::Tick);
parent.add_update_system(exchange, Phase::Couple);
parent.start();
```

For runtime-named cases (namespaces loaded from config), the string-keyed
`add_subapp` / `tick_subapp` and the [`Multi`](src/multi.rs) accessor are
the equivalents.

## Also exposed (escape hatches)

These are public for power-user / interop reasons but most code never
reaches for them:

- [`Multi`](src/multi.rs) — the string-keyed SystemParam behind
  `MultiRes` / `MultiResMut`. Its `read` / `write` (and panicking
  `expect_read` / `expect_write`) take a namespace string at runtime; use
  when the namespace is runtime data. Produces [`MultiRef`] / [`MultiMut`]
  deref handles.
- [`Transport`](src/transport.rs) trait + [`LocalTransport`](src/transport.rs)
  — implement `Transport` for a custom wire (e.g. TCP, ZeroMQ, shared
  memory). `LocalTransport::pair()` is in-memory channels for tests.
- [`Physics`](src/physics.rs) trait + `AppPhysics` + `RemoteMirrorPhysics`
  — the wrappers behind `add_subapp` / `add_remote_subapp`. Implement
  `Physics` directly if you need a sub-App backend that's neither.
- [`SubApps`](src/multi.rs) — the owning resource holding the registered
  sub-Apps; `participants()` / `any_done()` are useful from parent-App
  stop conditions.
- [`snapshot_subapp_resource`](src/snapshot.rs) /
  [`restore_subapp_resource`](src/snapshot.rs) — system constructors for
  save / restore of a sub-App's `T` resource (opt-in reversibility for
  Picard / adaptive retries), built on `grass_scheduler::Snapshot<T>`.

## See also

- [`grass_scheduler`](../grass_scheduler/) — the schedule / loop / branch
  primitives the parent App uses.
- [`grass_app`](../grass_app/) — the App / Plugin / Resource layer.

## License

MIT OR Apache-2.0
