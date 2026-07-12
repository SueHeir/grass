# Exported Child-Schedule Seams

An exported seam is a named point where a child `App` returns control to its
parent without completing the child's timestep. The parent can then run
coupling systems, diagnostics, synchronization, or policy decisions before it
resumes the child. This places coupling at a scientifically meaningful solver
boundary without calling back into the parent while the child scheduler is
borrowed.

The ownership rule is deliberate:

- the **child owns its numerical phases** and declares stable seam IDs;
- the **parent owns coupling policy**: ordering, convergence, retries, and
  synchronization;
- communication is an explicit parent action at a seam. Reaching a seam does
  not secretly perform a local copy, MPI exchange, or collective.

## Current support

As of the exported-seam implementation (`e09bd95`), seams may occur only
between direct children of the schedule's top-level `Sequence`. A child is
advanced with `advance_to_seam::<NS>(expected_id)` and finished with
`complete_subapp_step::<NS>()`. Both operations fail closed if the child yields
at a different seam or completes at an unexpected point.

```rust,ignore
let child_schedule = Schedule::builder()
    .then_variant(CfdPhase::Predict)
    .export_seam("cfd.interface_ready")
    .then_variant(CfdPhase::Correct)
    .build();

parent.add_update_system(
    advance_to_seam::<Cfd>("cfd.interface_ready"),
    ParentPhase::AdvanceCfd,
);
parent.add_update_system(exchange_interface, ParentPhase::Couple);
parent.add_update_system(
    complete_subapp_step::<Cfd>(),
    ParentPhase::CompleteCfd,
);
```

`advance_to_seam` temporarily takes `ResMut<SubApps>`, but that borrow ends
when the parent system returns. The following coupling system can therefore
use `MultiRes` / `MultiResMut` without re-entrant access to `SubApps`.

Seams nested in a `Loop`, `Branch`, or rollback fragment are currently rejected
when the schedule is installed. They require a recursive resumable cursor that
preserves loop iteration, selected branch, and rollback state across yields.
The examples below motivate that extension; they are a roadmap, not current API
claims.

## Why nested seams matter

### Partitioned implicit CFD-DEM coupling: a seam inside a loop

Strong drag or rapidly changing porosity can make a one-pass CFD-DEM exchange
unstable or inaccurate. A partitioned implicit step repeats the interface
solve until its force, velocity, or porosity residual converges:

```text
save beginning-of-step CFD and DEM states
repeat until interface residual converges:
    CFD predicts pressure/drag
    yield "cfd.interface_ready"
    parent exchanges drag, velocity, and porosity; applies relaxation
    DEM advances/responds
    yield "dem.interface_ready"
    parent exchanges particle motion and evaluates the coupled residual
commit the physical timestep
```

Here the seam belongs *inside* the fixed-point loop because every nonlinear
iteration needs an exchange. Putting the seam only outside the loop would turn
the method into a different, explicit algorithm. Fluidized beds with rapid
void-fraction changes are a representative CFD-DEM case. Added-mass-sensitive
immersed-boundary coupling is another: a loose exchange can be unstable even
when each standalone solver is stable.

Conceptually, the future schedule could read:

```rust,ignore
Schedule::builder()
    .loop_until(interface_converged, max_iters, OnMax::Panic, |iter| {
        iter.then_variant(CfdPhase::InterfaceSolve)
            .export_seam("cfd.interface_ready") // roadmap: nested seam
            .then_variant(DemPhase::Respond)
            .export_seam("dem.interface_ready") // roadmap: nested seam
            .then_variant(CoupledPhase::Residual)
    })
    .build()
```

### Regime-dependent algorithms: a seam inside a branch

A coupling policy may choose a different algorithm from current state:

- use a single explicit exchange while interaction is weak;
- enter implicit iterations when drag, added mass, or a residual indicates
  stiff coupling;
- switch between unresolved particle forcing and resolved immersed-boundary
  transfer as particle resolution changes;
- activate a fracture exchange only after a DEM-peridynamics damage criterion
  is met.

Only the selected branch should yield the seams required by that algorithm.
The resumable cursor must remember the chosen arm; re-evaluating the branch
condition after every resume could silently splice together two different
algorithms within one timestep.

### Adaptive rejection: a seam inside rollback

A coupled timestep may be rejected because a nonlinear solve diverges, a CFL
or contact limit is exceeded, penetration grows too large, or a conservation
check fails. Rollback is a multi-participant transaction:

```text
snapshot CFD, DEM, coupler history, and shared time
attempt the coupled timestep
if rejected:
    yield "coupling.restore"
    parent restores every participant to the same epoch
    parent selects and communicates the reduced shared dt
    yield "coupling.retry_ready"
    retry from the restored state
```

Neither participant may resume from rollback while another still holds
tentative state. Snapshot restoration must also include coupling history (for
example Aitken relaxation state), not just primary solver fields. A monotonically
identified coupling epoch prevents a delayed message from the rejected attempt
being mistaken for data from the retry.

## Other scientific uses

- **DEM-peridynamics fracture:** yield within damage/contact iterations so
  particle tractions, crack opening, and newly disconnected material are
  exchanged consistently; reject and retry when fracture growth invalidates
  the trial step.
- **TPS ablation and flow:** yield after heating/chemistry and after surface
  recession so the parent can transfer heat flux, update geometry, and request
  a flow remesh before either solver commits the step.
- **Multirate subcycling:** a fast DEM/contact solver may take many substeps
  inside one CFD step, with selected intermediate seams for forcing updates or
  stability checks rather than an exchange after every substep.

## Protocol invariants for recursive seams

Nested support should preserve these invariants:

1. **Stable identity.** Every boundary has an explicit, unique protocol ID;
   callers verify the expected ID and fail closed on mismatch.
2. **Exact continuation.** Resume continues after the yielded node with loop
   counters, branch choice, rollback phase, and local condition state intact.
3. **Parent-owned policy.** Children expose boundaries and data; the parent
   decides ordering, convergence, relaxation, retry, and timestep acceptance.
4. **Explicit synchronization.** A seam is control flow, not transport. Local
   copies, MPI exchanges, barriers, and epoch agreement are visible parent
   systems.
5. **Atomic rollback.** All participating solvers, shared time, coupling
   history, and communication epoch restore or commit together.
6. **No unreported crossing.** A resume operation returns at the next seam or
   at timestep completion; it never runs through a seam silently.

These rules make the parent schedule an inspectable statement of the coupled
algorithm. Child-level `MultiRes` remains available as an advanced integration
tool, but it does not replace explicit orchestration when convergence,
transport, or rollback spans multiple solvers.
