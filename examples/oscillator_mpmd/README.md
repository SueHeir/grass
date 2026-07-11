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

The launcher compares the four-state trajectory and its bit fingerprint with
the in-process `LocalTransport` counterpart.  It passes numerical trajectory
differences through `5e-14` and reports whether fingerprints match exactly.
The normal same-host build matches bit-for-bit.  A fingerprint difference is
legitimate on heterogeneous MPI ranks or builds that use different floating
point instruction paths; the numeric tolerance remains the acceptance rule.

![LocalTransport versus independent recurrence](plots/local_contract_comparison.png)

The points overlap: each LocalTransport remote mirror equals its peer's
exported final position to less than `5e-14` (PASS).
