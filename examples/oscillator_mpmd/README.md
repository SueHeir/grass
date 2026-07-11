# Two-binary MPI oscillator coupling

This example runs the existing `oscillator_demo` scientific library on two
independent ranks.  Each binary owns its local `OscillatorState`, velocity,
parameters, and integrator.  The only remote mirror resource is
`RemotePosition(f64)`, the explicit exchange contract used by the interface
spring.  It is packed with `Wire`, carried by `Transport`, and is deliberately
not a serialization of solver state.

The fixed `case.steps = 40` in [config.toml](config.toml) is the termination
contract shared by both ranks.  There is no unilateral stop message: both must
execute the same number of handshake and iteration pumps.

At setup, each rank exports its initial `RemotePosition` and receives the
other's.  Each iteration is ordered as:

1. Tick the local oscillator using its previously imported peer position.
2. Export the new local position into the remote mirror.
3. Tick the `RemoteSubApp`: send the mirror, then receive the peer mirror.
4. Import that received position into local `PeerPosition` for the next tick.

The named export stage is important: `RemoteMirrorPhysics` sends the mirror's
resource cell, not the local solver state.  Skipping it silently sends stale
data.  Failed receives and malformed packets carry the mirror name, setup or
each-iteration phase, direction, slot number, payload length, and underlying
transport/wire cause through the existing fallible remote-pump diagnostic.

The setup exchange establishes the other side's initial position before the
first local tick.  At every iteration pump, each rank then imports the other
rank's freshly exported position for its next local tick.  The independent
recurrence used below implements this same exchange order for both the
`LocalTransport` replay and the MPI launch.

## Runs

Ordinary CI exercises the identical two-sided composition in two threads using
`LocalTransport`:

```bash
cargo test --example oscillator_mpmd_local
cargo run --example oscillator_mpmd_local
```

On an MPI runner, build and launch the two binaries in this exact order:

```bash
cargo build --features mpi --example oscillator_mpmd_a --example oscillator_mpmd_b
./examples/oscillator_mpmd/run_mpmd.sh
```

`oscillator_mpmd_a` requires absolute `MPI_COMM_WORLD` rank 0 and addresses
rank 1; `oscillator_mpmd_b` requires rank 1 and addresses rank 0.  The
transport uses raw WORLD ranks, so these assumptions remain true even if a
larger solver later calls `init_app_color` to obtain its own intra-app
communicator.  Any other MPMD rank layout fails at startup with the expected
and actual rank context.

The launcher records all 40 `(A.x, A.v, B.x, B.v)` states from both binaries,
then compares the complete MPI and `LocalTransport` trajectories independently
with the explicit recurrence used by [sweep.py](sweep.py).  Each comparison,
including MPI-versus-local parity, must stay within `5e-14`; missing,
duplicate, or out-of-order trace lines fail with a rank/handshake/export-stage
diagnostic.  It additionally reports the final IEEE-754 fingerprint.  A
fingerprint mismatch is a documented warning—not a false failure—when every
trajectory value satisfies the numerical criterion, because heterogeneous MPI
ranks or builds may take different floating-point instruction paths.

![LocalTransport full state trajectory versus independent recurrence](plots/local_contract_comparison.png)

The four panels show the full 40-step LocalTransport state vector—A position,
A velocity, B position, and B velocity—against the separately implemented
explicit recurrence.  Every checked observable overlaps the visible
±`5e-14` criterion (PASS).  The MPI CI launch applies that same full-vector
recurrence comparison to its recorded two-binary trajectory.
