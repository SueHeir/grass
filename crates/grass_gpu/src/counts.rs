//! A running tally of CPU↔GPU traffic.

use std::sync::atomic::{AtomicU64, Ordering};

/// Counts every transfer and queue drain on one [`GpuDevice`](crate::GpuDevice).
///
/// This counts events and bytes, not time. The atomics keep the device `Send + Sync`
/// while still recording through a shared reference.
#[derive(Debug, Default)]
pub struct TransferCounts {
    uploads: AtomicU64,
    upload_bytes: AtomicU64,
    downloads: AtomicU64,
    download_bytes: AtomicU64,
    waits: AtomicU64,
    batches: AtomicU64,
}

/// A plain, comparable copy of [`TransferCounts`] at one instant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransferSnapshot {
    /// CPU→GPU transfers.
    pub uploads: u64,
    /// Bytes moved CPU→GPU.
    pub upload_bytes: u64,
    /// GPU→CPU transfers.
    pub downloads: u64,
    /// Bytes moved GPU→CPU.
    pub download_bytes: u64,
    /// Blocking waits for the queue to drain, downloads excluded.
    pub waits: u64,
    /// Groups of downloads that shared one wait.
    pub batches: u64,
}

impl TransferSnapshot {
    /// What happened between `earlier` and this snapshot.
    pub fn since(&self, earlier: &TransferSnapshot) -> TransferSnapshot {
        TransferSnapshot {
            uploads: self.uploads - earlier.uploads,
            upload_bytes: self.upload_bytes - earlier.upload_bytes,
            downloads: self.downloads - earlier.downloads,
            download_bytes: self.download_bytes - earlier.download_bytes,
            waits: self.waits - earlier.waits,
            batches: self.batches - earlier.batches,
        }
    }
}

impl TransferCounts {
    /// Records one CPU→GPU transfer of `bytes` bytes.
    pub fn record_upload(&self, bytes: u64) {
        self.uploads.fetch_add(1, Ordering::Relaxed);
        self.upload_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Records one GPU→CPU transfer of `bytes` bytes.
    pub fn record_download(&self, bytes: u64) {
        self.downloads.fetch_add(1, Ordering::Relaxed);
        self.download_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Records one blocking wait for the queue to drain.
    pub fn record_wait(&self) {
        self.waits.fetch_add(1, Ordering::Relaxed);
    }

    /// Records one group of downloads sharing a single wait.
    pub fn record_batch(&self) {
        self.batches.fetch_add(1, Ordering::Relaxed);
    }

    /// Takes a comparable copy of the current tallies.
    pub fn snapshot(&self) -> TransferSnapshot {
        TransferSnapshot {
            uploads: self.uploads.load(Ordering::Relaxed),
            upload_bytes: self.upload_bytes.load(Ordering::Relaxed),
            downloads: self.downloads.load(Ordering::Relaxed),
            download_bytes: self.download_bytes.load(Ordering::Relaxed),
            waits: self.waits.load(Ordering::Relaxed),
            batches: self.batches.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_accumulate_and_diff() {
        let counts = TransferCounts::default();
        counts.record_upload(16);
        let start = counts.snapshot();
        counts.record_upload(8);
        counts.record_download(4);
        counts.record_wait();
        assert_eq!(
            counts.snapshot().since(&start),
            TransferSnapshot {
                uploads: 1,
                upload_bytes: 8,
                downloads: 1,
                download_bytes: 4,
                waits: 1,
                batches: 0,
            }
        );
    }
}
