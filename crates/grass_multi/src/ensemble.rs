//! Ensembles: many independent runs of the same model.
//!
//! Uncertainty quantification, parameter sweeps and sensitivity studies all
//! run one model many times with different inputs. An [`Ensemble`] hands each
//! member a [`Member`] (its index and a reproducible seed), runs the members
//! on a pool of threads, and returns their results in member order. Each
//! member typically builds and runs its own grass `App`, so members share
//! nothing.
//!
//! ```
//! use grass_multi::Ensemble;
//!
//! // 64 members of a toy model; each draws its parameter from its own seed.
//! let results = Ensemble::new(64).seed(7).threads(4).run(|m| {
//!     let k = 1.0 + m.uniform(0) ;            // a parameter in [1, 2)
//!     (-k).exp()                              // the model's output
//! });
//! assert_eq!(results.len(), 64);
//! let mean = results.iter().sum::<f64>() / 64.0;
//! assert!(mean > (-2.0f64).exp() && mean < (-1.0f64).exp());
//! ```
//!
//! Across MPI ranks, give each rank its share with [`Ensemble::run_rank`]
//! and collect outputs on rank 0 with `gather_outputs` (feature `mpi`).
//! [`Summary`] accumulates mean, variance, minimum and maximum per output
//! component without storing every sample.

use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// One ensemble member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    /// Index in `0..count`.
    pub index: usize,
    /// Ensemble size.
    pub count: usize,
    /// A seed unique to this member and the ensemble seed. The same
    /// `(ensemble seed, index)` always gives the same seed, whichever thread
    /// or rank runs the member.
    pub seed: u64,
}

impl Member {
    /// A reproducible uniform number in `[0, 1)`: the `k`-th draw of this
    /// member's stream. Enough to place a member in a parameter space;
    /// use `seed` with a real RNG for anything more.
    pub fn uniform(&self, k: u64) -> f64 {
        (splitmix64(self.seed ^ splitmix64(k.wrapping_add(0x5851_f42d_4c95_7f2d))) >> 11) as f64
            / (1u64 << 53) as f64
    }
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The members owned by `rank` of `size` under a block distribution: as
/// even as possible, lower ranks taking one extra when it does not divide.
pub fn partition(count: usize, rank: usize, size: usize) -> Range<usize> {
    assert!(size > 0 && rank < size, "rank {rank} out of {size}");
    let base = count / size;
    let extra = count % size;
    let start = rank * base + rank.min(extra);
    start..start + base + usize::from(rank < extra)
}

/// A set of independent model runs.
#[derive(Debug, Clone, Copy)]
pub struct Ensemble {
    count: usize,
    seed: u64,
    threads: usize,
}

impl Ensemble {
    /// `count` members, seed 0, one thread per available core.
    pub fn new(count: usize) -> Self {
        Self {
            count,
            seed: 0,
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        }
    }

    /// The ensemble seed every member seed derives from.
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Number of worker threads (at least 1).
    pub fn threads(mut self, n: usize) -> Self {
        self.threads = n.max(1);
        self
    }

    /// Number of members.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the ensemble has no members.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Member `index`.
    pub fn member(&self, index: usize) -> Member {
        Member {
            index,
            count: self.count,
            seed: splitmix64(self.seed ^ splitmix64(index as u64)),
        }
    }

    /// Run every member; results in member order.
    pub fn run<R: Send>(&self, f: impl Fn(Member) -> R + Sync) -> Vec<R> {
        self.run_range(0..self.count, f)
    }

    /// Run the members `rank` owns under [`partition`]; returns
    /// `(index, result)` pairs in member order.
    pub fn run_rank<R: Send>(
        &self,
        rank: usize,
        size: usize,
        f: impl Fn(Member) -> R + Sync,
    ) -> Vec<(usize, R)> {
        let range = partition(self.count, rank, size);
        let start = range.start;
        self.run_range(range, f)
            .into_iter()
            .enumerate()
            .map(|(i, r)| (start + i, r))
            .collect()
    }

    fn run_range<R: Send>(&self, range: Range<usize>, f: impl Fn(Member) -> R + Sync) -> Vec<R> {
        let n = range.len();
        let slots: Mutex<Vec<Option<R>>> = Mutex::new((0..n).map(|_| None).collect());
        let next = AtomicUsize::new(0);
        let workers = self.threads.min(n).max(1);
        std::thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let r = f(self.member(range.start + i));
                    slots.lock().expect("a member panicked")[i] = Some(r);
                });
            }
        });
        slots
            .into_inner()
            .expect("a member panicked")
            .into_iter()
            .map(|r| r.expect("every member ran"))
            .collect()
    }
}

/// Running mean, variance, minimum and maximum of vector outputs
/// (Welford's algorithm, per component).
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    n: u64,
    mean: Vec<f64>,
    m2: Vec<f64>,
    min: Vec<f64>,
    max: Vec<f64>,
}

impl Summary {
    /// An empty summary of `ncomp`-component outputs.
    pub fn new(ncomp: usize) -> Self {
        Self {
            n: 0,
            mean: vec![0.0; ncomp],
            m2: vec![0.0; ncomp],
            min: vec![f64::INFINITY; ncomp],
            max: vec![f64::NEG_INFINITY; ncomp],
        }
    }

    /// Summarize a set of outputs.
    pub fn of<'a>(outputs: impl IntoIterator<Item = &'a [f64]>) -> Option<Self> {
        let mut it = outputs.into_iter().peekable();
        let mut s = Self::new(it.peek()?.len());
        for o in it {
            s.add(o);
        }
        Some(s)
    }

    /// Add one output.
    ///
    /// # Panics
    /// Panics if `x` has the wrong number of components.
    pub fn add(&mut self, x: &[f64]) {
        assert_eq!(
            x.len(),
            self.mean.len(),
            "output has the wrong number of components"
        );
        self.n += 1;
        let n = self.n as f64;
        for (k, &v) in x.iter().enumerate() {
            let d = v - self.mean[k];
            self.mean[k] += d / n;
            self.m2[k] += d * (v - self.mean[k]);
            self.min[k] = self.min[k].min(v);
            self.max[k] = self.max[k].max(v);
        }
    }

    /// Number of outputs added.
    pub fn count(&self) -> u64 {
        self.n
    }
    /// Mean per component.
    pub fn mean(&self) -> &[f64] {
        &self.mean
    }
    /// Unbiased sample variance per component (zero with fewer than two outputs).
    pub fn variance(&self) -> Vec<f64> {
        let d = (self.n.max(2) - 1) as f64;
        self.m2
            .iter()
            .map(|m| if self.n < 2 { 0.0 } else { m / d })
            .collect()
    }
    /// Standard deviation per component.
    pub fn std_dev(&self) -> Vec<f64> {
        self.variance().into_iter().map(f64::sqrt).collect()
    }
    /// Standard error of the mean per component.
    pub fn std_error(&self) -> Vec<f64> {
        let n = (self.n.max(1)) as f64;
        self.std_dev().into_iter().map(|s| s / n.sqrt()).collect()
    }
    /// Minimum per component.
    pub fn min(&self) -> &[f64] {
        &self.min
    }
    /// Maximum per component.
    pub fn max(&self) -> &[f64] {
        &self.max
    }
}

/// Collect every rank's `(index, output)` pairs on rank 0, in member order.
/// Returns `Some(outputs)` on rank 0 (`outputs[i]` is member `i`'s output)
/// and `None` elsewhere. Collective: every rank must call it.
///
/// # Panics
/// On rank 0, if any member of `0..count` is missing.
#[cfg(feature = "mpi")]
pub fn gather_outputs(
    comm: &dyn grass_mpi::CommBackend,
    count: usize,
    local: Vec<(usize, Vec<f64>)>,
) -> Option<Vec<Vec<f64>>> {
    let pack = |pairs: &[(usize, Vec<f64>)]| {
        let mut buf = vec![pairs.len() as f64];
        for (i, v) in pairs {
            buf.push(*i as f64);
            buf.push(v.len() as f64);
            buf.extend_from_slice(v);
        }
        buf
    };
    if comm.rank() != 0 {
        comm.send_f64(0, &pack(&local));
        return None;
    }
    let mut out: Vec<Option<Vec<f64>>> = vec![None; count];
    for (i, v) in local {
        out[i] = Some(v);
    }
    for src in 1..comm.size() {
        let buf = comm.recv_f64(src);
        let mut p = 1;
        for _ in 0..buf[0] as usize {
            let (i, len) = (buf[p] as usize, buf[p + 1] as usize);
            out[i] = Some(buf[p + 2..p + 2 + len].to_vec());
            p += 2 + len;
        }
    }
    Some(
        out.into_iter()
            .enumerate()
            .map(|(i, v)| v.unwrap_or_else(|| panic!("ensemble member {i} has no output")))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_covers_every_member_once() {
        for (count, size) in [(10, 3), (3, 5), (0, 2), (64, 8), (7, 1)] {
            let mut seen = Vec::new();
            for r in 0..size {
                let p = partition(count, r, size);
                assert!(p.len() <= count.div_ceil(size));
                seen.extend(p);
            }
            assert_eq!(seen, (0..count).collect::<Vec<_>>(), "{count} over {size}");
        }
    }

    #[test]
    fn results_and_seeds_do_not_depend_on_threads_or_ranks() {
        let model = |m: Member| (m.index, m.seed, m.uniform(3));
        let one = Ensemble::new(37).seed(11).threads(1).run(model);
        let many = Ensemble::new(37).seed(11).threads(8).run(model);
        assert_eq!(one, many);
        let by_rank: Vec<_> = (0..4)
            .flat_map(|r| Ensemble::new(37).seed(11).threads(3).run_rank(r, 4, model))
            .map(|(_, x)| x)
            .collect();
        assert_eq!(by_rank, one);
        // Different ensemble seeds give different streams.
        let other = Ensemble::new(37).seed(12).run(model);
        assert_ne!(other[0].1, one[0].1);
        // Uniform draws are in [0, 1) and look uniform.
        let u: Vec<f64> = Ensemble::new(4000).run(|m| m.uniform(0));
        assert!(u.iter().all(|x| (0.0..1.0).contains(x)));
        let mean = u.iter().sum::<f64>() / u.len() as f64;
        assert!((mean - 0.5).abs() < 0.02, "mean {mean}");
    }

    #[test]
    fn summary_matches_two_pass_statistics() {
        let xs: Vec<[f64; 2]> = (0..101)
            .map(|i| [i as f64 * 0.1, (i as f64).sin()])
            .collect();
        let s = Summary::of(xs.iter().map(|x| &x[..])).unwrap();
        for k in 0..2 {
            let n = xs.len() as f64;
            let mean = xs.iter().map(|x| x[k]).sum::<f64>() / n;
            let var = xs.iter().map(|x| (x[k] - mean).powi(2)).sum::<f64>() / (n - 1.0);
            assert!((s.mean()[k] - mean).abs() < 1e-12);
            assert!((s.variance()[k] - var).abs() < 1e-12);
        }
        assert_eq!(s.count(), 101);
        assert_eq!(s.min()[0], 0.0);
        assert!((s.max()[0] - 10.0).abs() < 1e-12);
    }
}
