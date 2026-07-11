# Scientific Library Composition Contract

This is the contract for a library that calls itself **Grass-compatible**. It
applies to a solver, substrate, physics package, I/O package, or coupling
package built on Grass. The words **MUST**, **MUST NOT**, **SHOULD**, and
**SHOULD NOT** are normative. Everything after [How to use this](#how-to-use-this)
is explanatory.

This contract describes the behavior available in this repository today. It
does not claim portability to parallel dispatch, general distributed rollback,
or a stable external wire protocol.

## Normative contract

### 1. Ownership and declared access

- A library **MUST** own its simulation state as typed App resources; a system
  **MUST** declare each resource it reads as `Res<T>` and each it mutates as
  `ResMut<T>`. `Local<T>` is private state of one system, not shared solver
  state. The [scheduler access test](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_scheduler/src/lib.rs#L286-L320)
  exercises the declared access inventory.
- A library **MUST NOT** rely on undeclared shared state to communicate between
  systems. It **MUST** establish `.before()`, `.after()`, or phase order when a
  reader must observe a particular writer; conflicting same-phase access is not
  rejected by schedule validation. The [typed-label example](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/examples/typed_system_labels/main.rs#L62-L109)
  checks required and optional ordering.
- A package for this tier **MUST NOT** embed a discretization assumption in
  Grass-facing abstractions. Particle, mesh, and equation-specific state belongs
  in a lower or physics tier, not `grass_*`. The runnable
  [heat-diffusion example](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/examples/heat_diffusion_1d/main.rs#L65-L160) is the
  current non-particle scheduler evidence.

### 2. Systems, labels, and time

- A library **MUST** register each repeated operation as an update system and
  one-time initialization as a setup system. It **SHOULD** use a
  `ScheduleSet` enum for ordered phases, treating declaration order as an API
  contract. A system needing a label **SHOULD** use an exported typed
  `SystemLabel`/`SystemKey`, and **MUST** use `.requires(...)` when absence is
  an error; `.after(...)` is only an optional order preference. The [label
  matrix](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/examples/typed_system_labels/main.rs#L62-L109) covers both cases.
- A composed app **MUST** give independently authored phase enums distinct
  namespaces, or install an explicit `Schedule`; otherwise their index-zero
  phases may interleave. The [flat-order and installed-schedule tests](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_scheduler/tests/schedule.rs#L234-L272)
  demonstrate the namespace-zero tie-break and explicit schedule order.
- A library with stage-specific behavior **MUST** make stage names and order
  agree between `StageEnum` and declarative `[[run]]` configuration. It
  **MUST NOT** treat a bare scheduler's `stage_name` as driven. Startup
  validation is described and exercised by the [stage contract](derives.md#stageenum).
- An external driver that uses `prepare()`/`run()` rather than `start()`
  **MUST** call `run_cleanup()`; a parent that owns sub-Apps **MUST** call
  `SubApps::cleanup_all()`. This is observable lifecycle behavior, not an
  automatic transitive cleanup. The [lifecycle implementation](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_app/src/app.rs#L454-L484)
  and [sub-App termination test](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_multi/tests/multi_phase0.rs#L48-L64)
  document both paths.

### 3. Plugins and capabilities

- A library feature **SHOULD** be a `Plugin` or `PluginGroup` that registers
  only the resources and systems it owns. A plugin that cannot build without a
  concrete plugin already installed **MUST** declare its `TypeId` dependency.
  The [dependency tests](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_app/src/app.rs#L1081-L1134)
  cover a satisfied and a missing concrete dependency.
- A substitutable service **MUST** be expressed as an exported `CapabilityId`;
  consumers **MUST** declare required capabilities and providers **MUST**
  declare provided capabilities. Callers that need recoverable diagnostics
  **MUST** use `try_prepare`, `try_start`, or
  `validate_capability_contracts_result`, rather than a panicking convenience
  path. The [capability tests](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_app/src/app.rs#L1216-L1259)
  cover missing, alternative, and duplicate providers.
- Plugin default configuration **MUST** be declarative TOML returned by
  `default_config`; it **MUST NOT** be a script that mutates registration or
  schedule structure. The [`Plugin::default_config` contract](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_app/src/plugin.rs#L157-L163)
  and its [collection during plugin registration](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_app/src/app.rs#L308-L326)
  are exercised by the runnable [observed-oscillator example](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/examples/observed_oscillator/main.rs#L1-L17).

### 4. Coupling ownership and exchange ports

- A coupling between two independently useful solver/substrate tiers **MUST**
  be owned by the coupling package or parent application that owns their seam;
  neither participant should import the other's internal state merely to
  couple. The coupling owner **MUST** own the parent schedule and termination
  policy. The [parent/sub-App integration test](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_multi/tests/multi_phase0.rs#L91-L195)
  demonstrates a parent owning the tick, exchange, and stop policy.
- A stable exchange **MUST** use `Port<T>` and a contract type owned by the
  interface, with `expose_field` and `consume_field`; the consumer **MUST NOT**
  name the producer's private resource type. The producer-to-port-to-consumer
  sequence **MUST** be ordered around producer and consumer ticks. The
  [port integration test](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_multi/tests/coupling_port.rs#L1-L178)
  drives a field-style producer and particle-style consumer against a closed
  form, and also checks independent ports compose.
- A one-off `MultiRes`/`MultiResMut` coupler **MAY** read/write sub-App state
  directly, but ticking and `Multi*` access **MUST NOT** share one system:
  their incompatible `SubApps` borrows panic at runtime. [Runnable tutorial
  example](../tutorial/coupling-two-solvers.md#6-write-the-coupler) shows the
  separate Tick and Couple phases.

### 5. Remote coupling, Wire, and errors

- A remote coupling **MUST** declare every transferred resource's direction and
  cadence (`send_at_setup`, `recv_at_setup`, `send_each_iter`, or
  `recv_each_iter`) on both peers in matching order. Before a peer tick,
  the local side **MUST** export its current value into the mirror; otherwise
  the peer can observe stale bounced-back state. The [remote wiring tests](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_multi/tests/multi_phase3.rs#L428-L670)
  cover both the stale and correctly exported cases.
- A remote payload type **MUST** implement `Wire`; `pack` and `try_unpack`
  **MUST** agree on exactly one complete transport message. Implementations
  **MUST** reject malformed input through `WireUnpackError` rather than relying
  on a panic. Primitive and malformed-payload coverage lives in
  [wire tests](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_multi/src/wire.rs#L278-L358).
- A `Transport` implementation **MUST** make `try_send` and `try_recv` return
  `TransportError` with its name, operation, and useful failure detail.
  Remote callers **SHOULD** preserve the resulting mirror, phase, direction,
  slot, and decoding context. The [transport/error integration tests](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_multi/tests/multi_phase3.rs#L711-L870)
  check peer drops, send/receive failures, truncation, and invalid UTF-8.

### 6. Coherence and determinism

- A library that mirrors a resource across host and device **MUST** register a
  `CoherenceRegistry` mirror and declare every system access accurately. The
  device producer **MUST** handle host-dirty upload before it advances and mark
  the mirror device-dirty after it advances. Host consumers then trigger the
  scheduler's lazy download. The [coherence tests](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_scheduler/src/coherence.rs#L296-L358)
  cover the state transitions and forced reads.
- Grass execution is currently single-threaded and deterministic: phase/
  namespace order, explicit dependencies, then registration order decide the
  sequence. Libraries **MUST NOT** infer a parallel-execution or race-freedom
  guarantee from this. The [flat-order test](http://192.168.0.170:8082/SueHeir/grass/src/commit/a383502c23c1c07e7f92b67f3afab455e9dbb692/crates/grass_scheduler/tests/schedule.rs#L234-L251)
  exercises the observable registration-order trace.

## Author checklist

- [ ] All shared solver state is a typed resource; every system declares its accesses.
- [ ] Phases, typed labels, and cross-system dependencies state the intended order.
- [ ] Schedule namespaces or an explicit schedule separate independently authored solvers.
- [ ] Lifecycle, stages, and cleanup are owned and explicitly driven.
- [ ] Plugins declare concrete dependencies and exported capability contracts.
- [ ] Configuration is declarative TOML.
- [ ] A coupling owns the seam; stable data crosses a `Port<T>`, not private state types.
- [ ] Remote transfer has matched direction/cadence, `Wire` validation, and fallible errors.
- [ ] Device mirrors register coherence and honor dirty-state transitions.
- [ ] Documentation links a runnable example or test for each public claim and labels unproven behavior as such.

## How to use this

Start by drawing the ownership boundary: one solver owns its resources and
systems; the parent owns only the schedule and explicit exchange contract.
Then make time visible: use phases for causal order, typed labels for named
dependencies, and stages only when declarative run configuration drives them.

For a local pair, the usual shape is `TickProducer → Expose → Consume →
TickConsumer → Check`. For a remote pair, insert an export to the remote mirror
before the peer's tick and use matching wire-pump declarations on both sides.
The [coupling tutorial](../tutorial/coupling-two-solvers.md) expands both cases.

## Reserved and unproven behavior

- `StepResult::completed_full_step` is reserved for a future full-step versus
  substep distinction; current local and remote adapters do not use it.
- `Wire` is deliberately small and hand-written. There is no version-negotiated
  protocol, serde blanket implementation, derive, framing layer, or guarantee
  of compatibility across independently evolved binaries.
- Coherence is a host/device hook with unit coverage, not proof of a production
  GPU backend or distributed-memory coherence.
- Determinism is the current sequential scheduler contract; parallel execution
  and reproducibility across different transports, platforms, or floating-point
  implementations are not established here.
