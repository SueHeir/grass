//! Where in a timestep host and device state must agree, and how often that
//! agreement had to be bought with a transfer.
//!
//! This module holds the parts of the compute abstraction that are *not*
//! CubeCL-dependent, so a downstream crate can name a sync point, or read a
//! transfer count, in code that compiles with the `cubecl` feature off.

use grass_derive::ScheduleSet;
use std::sync::atomic::{AtomicU64, Ordering};

/// Named sync points for one timestep of a device-accelerated schedule.
///
/// The migration's real contract is not "which kernels run on the GPU" but
/// "at which points in the step is the host copy allowed to be stale". These
/// four sets are that contract, written down where the scheduler can order
/// against it. GRASS itself registers nothing physical into them — a physics
/// tier decides what goes where.
///
/// Read the variants as a statement about *state*, not about *work*:
///
/// | Set | After this set runs … |
/// |---|---|
/// | [`HostToDevice`](Self::HostToDevice) | the device copy reflects every host write made this step |
/// | [`DeviceCompute`](Self::DeviceCompute) | the device copy is the authoritative one; the host copy is stale |
/// | [`DeviceToHost`](Self::DeviceToHost) | the device results have been pulled back |
/// | [`HostCoherent`](Self::HostCoherent) | host and device agree, and the device queue is drained |
///
/// [`HostCoherent`](Self::HostCoherent) is the one that matters for this stack specifically.
/// Parallelism here is MPI domain decomposition, and MPI wants host pointers,
/// so every ghost/halo exchange, every collective, and every dump has to sit
/// at or after `HostCoherent`. Putting a `forward_comm` in `DeviceCompute`
/// would read a stale host buffer and be silently wrong rather than a crash.
///
/// # Ordering footgun
///
/// `ScheduleSet` indices are per-enum and the namespace defaults to `0` for
/// every enum, so these four sets will *interleave* with a solver's own phase
/// enum unless the application separates them — see
/// [`grass_scheduler::chain_namespaces!`] or
/// [`Scheduler::set_schedule`](grass_scheduler::Scheduler::set_schedule).
/// Interleaving is not reported as an error, it just runs in the wrong order,
/// which for a coherence contract means "wrong answers, no diagnostic".
///
/// # Variant order is load-bearing
///
/// `#[derive(ScheduleSet)]` numbers variants by declaration order. Reordering
/// them reorders the schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ScheduleSet)]
pub enum ComputeSyncSet {
    /// Host→device uploads. Systems that push host-side state onto the device
    /// belong here.
    HostToDevice,
    /// Device-resident work: kernel launches. The host copy of anything the
    /// kernels write is stale from here until [`DeviceToHost`](Self::DeviceToHost).
    DeviceCompute,
    /// Device→host downloads. Systems that pull results back belong here.
    DeviceToHost,
    /// Coherence barrier. The device queue is drained and both copies agree.
    /// MPI exchange, thermo output and dumps go at or after this point.
    HostCoherent,
}

/// Running tally of host↔device traffic and device barriers.
///
/// The migration plan asks that "the number of transfers per step is explicit
/// and countable, so it can be measured the day hardware exists". This is that
/// counter. It counts *events and bytes*, not time: nothing here is a
/// performance measurement, and on a box with no discrete GPU it could not be.
///
/// Counters are atomic so a `ComputeDevice` holding one
/// stays `Send + Sync` while still recording through a shared reference.
#[derive(Debug, Default)]
pub struct TransferCounts {
    host_to_device: AtomicU64,
    host_to_device_bytes: AtomicU64,
    device_to_host: AtomicU64,
    device_to_host_bytes: AtomicU64,
    barriers: AtomicU64,
}

/// A plain, comparable copy of [`TransferCounts`] taken at one instant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransferSnapshot {
    /// Number of completed host→device transfers.
    pub host_to_device: u64,
    /// Bytes moved host→device.
    pub host_to_device_bytes: u64,
    /// Number of completed device→host transfers.
    pub device_to_host: u64,
    /// Bytes moved device→host.
    pub device_to_host_bytes: u64,
    /// Number of blocking device barriers (queue drains).
    pub barriers: u64,
}

impl TransferCounts {
    /// A fresh, all-zero tally.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one host→device transfer of `bytes` bytes.
    pub fn record_host_to_device(&self, bytes: u64) {
        self.host_to_device.fetch_add(1, Ordering::Relaxed);
        self.host_to_device_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Records one device→host transfer of `bytes` bytes.
    pub fn record_device_to_host(&self, bytes: u64) {
        self.device_to_host.fetch_add(1, Ordering::Relaxed);
        self.device_to_host_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Records one blocking device barrier.
    pub fn record_barrier(&self) {
        self.barriers.fetch_add(1, Ordering::Relaxed);
    }

    /// Takes a comparable copy of the current tallies.
    ///
    /// The five loads are independent, so a snapshot taken while another
    /// thread is recording can straddle an update. Every caller in this stack
    /// is single-threaded per rank, which is the same promise `grass_mpi`
    /// relies on.
    pub fn snapshot(&self) -> TransferSnapshot {
        TransferSnapshot {
            host_to_device: self.host_to_device.load(Ordering::Relaxed),
            host_to_device_bytes: self.host_to_device_bytes.load(Ordering::Relaxed),
            device_to_host: self.device_to_host.load(Ordering::Relaxed),
            device_to_host_bytes: self.device_to_host_bytes.load(Ordering::Relaxed),
            barriers: self.barriers.load(Ordering::Relaxed),
        }
    }

    /// Resets every tally to zero (e.g. at the start of a measured window).
    pub fn reset(&self) {
        self.host_to_device.store(0, Ordering::Relaxed);
        self.host_to_device_bytes.store(0, Ordering::Relaxed);
        self.device_to_host.store(0, Ordering::Relaxed);
        self.device_to_host_bytes.store(0, Ordering::Relaxed);
        self.barriers.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grass_scheduler::ScheduleSet as _;

    #[test]
    fn sync_set_indices_follow_declaration_order() {
        assert_eq!(ComputeSyncSet::HostToDevice.to_index(), 0);
        assert_eq!(ComputeSyncSet::DeviceCompute.to_index(), 1);
        assert_eq!(ComputeSyncSet::DeviceToHost.to_index(), 2);
        assert_eq!(ComputeSyncSet::HostCoherent.to_index(), 3);
    }

    #[test]
    fn sync_set_names_are_stable() {
        assert_eq!(ComputeSyncSet::HostToDevice.name(), "HostToDevice");
        assert_eq!(ComputeSyncSet::HostCoherent.name(), "HostCoherent");
    }

    #[test]
    fn counts_accumulate_and_reset() {
        let counts = TransferCounts::new();
        counts.record_host_to_device(16);
        counts.record_host_to_device(8);
        counts.record_device_to_host(4);
        counts.record_barrier();

        assert_eq!(
            counts.snapshot(),
            TransferSnapshot {
                host_to_device: 2,
                host_to_device_bytes: 24,
                device_to_host: 1,
                device_to_host_bytes: 4,
                barriers: 1,
            }
        );

        counts.reset();
        assert_eq!(counts.snapshot(), TransferSnapshot::default());
    }
}
