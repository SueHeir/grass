//! Waveforms: interface data over a coupling time window, for subcycling.
//!
//! When coupled solvers take different time steps, exchanging one value per
//! window forces the faster solver to hold its input constant across its
//! substeps, which costs an order of accuracy in time. A [`Waveform`] keeps
//! every sample the producer computed during the window and lets the
//! consumer interpolate at its own substep times (waveform iteration, as in
//! Rüth et al. 2021 and preCICE v3).
//!
//! A waveform is ordinary data: the producer pushes a sample after each of
//! its substeps, the waveform travels through a port like any other value,
//! and the consumer calls [`Waveform::sample`] at its own times. For
//! implicit coupling, the samples after the first are the iterated interface
//! values ([`Waveform::interior`] and [`Waveform::set_interior`]).
//!
//! ```
//! use grass_multi::{Interpolation, Waveform};
//!
//! // A solver took two substeps over the window [1.0, 1.5].
//! let mut w = Waveform::new(1);
//! w.start(1.0, &[10.0]);
//! w.push(1.25, &[12.0]);
//! w.push(1.5, &[13.0]);
//!
//! // A solver with three substeps samples it at its own times.
//! let t = 1.0 + 0.5 / 3.0;
//! let v = w.sample(t, Interpolation::Linear)[0];
//! assert!((v - (10.0 + 2.0 * (t - 1.0) / 0.25)).abs() < 1e-12);
//! // Without interpolation it would see the next stored value.
//! assert_eq!(w.sample(t, Interpolation::Constant)[0], 12.0);
//! ```

/// How a [`Waveform`] is evaluated between samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interpolation {
    /// The first sample at or after `t`. With only the window ends stored
    /// this is the window-end value, which is what a partitioned code without
    /// time interpolation hands to every substep. First order in time.
    Constant,
    /// Piecewise linear between samples. Second order in time.
    #[default]
    Linear,
}

/// Samples of an `ncomp`-component quantity at increasing times.
#[derive(Debug, Clone, PartialEq)]
pub struct Waveform {
    ncomp: usize,
    times: Vec<f64>,
    values: Vec<f64>,
}

impl Waveform {
    /// An empty waveform of `ncomp` components per sample.
    ///
    /// # Panics
    /// Panics if `ncomp == 0`.
    pub fn new(ncomp: usize) -> Self {
        assert!(ncomp > 0, "ncomp must be positive");
        Self {
            ncomp,
            times: Vec::new(),
            values: Vec::new(),
        }
    }

    /// Discard all samples and start again with `v` at time `t`.
    pub fn start(&mut self, t: f64, v: &[f64]) {
        self.times.clear();
        self.values.clear();
        self.push(t, v);
    }

    /// Start a new window at the current end time, keeping only the last
    /// sample (the converged state the next window starts from).
    ///
    /// # Panics
    /// Panics if the waveform is empty.
    pub fn restart_from_end(&mut self) {
        let (t, v) = self.end();
        let v = v.to_vec();
        self.start(t, &v);
    }

    /// Append a sample.
    ///
    /// # Panics
    /// Panics if `v.len() != ncomp` or `t` is not later than the last sample.
    pub fn push(&mut self, t: f64, v: &[f64]) {
        assert_eq!(
            v.len(),
            self.ncomp,
            "sample has the wrong number of components"
        );
        if let Some(&last) = self.times.last() {
            assert!(t > last, "waveform times must increase ({t} after {last})");
        }
        self.times.push(t);
        self.values.extend_from_slice(v);
    }

    /// Components per sample.
    pub fn ncomp(&self) -> usize {
        self.ncomp
    }

    /// Number of samples.
    pub fn len(&self) -> usize {
        self.times.len()
    }

    /// Whether there are no samples.
    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// Sample times.
    pub fn times(&self) -> &[f64] {
        &self.times
    }

    /// Sample `i`.
    pub fn value(&self, i: usize) -> &[f64] {
        &self.values[i * self.ncomp..(i + 1) * self.ncomp]
    }

    /// The last sample.
    ///
    /// # Panics
    /// Panics if the waveform is empty.
    pub fn end(&self) -> (f64, &[f64]) {
        let i = self.len().checked_sub(1).expect("waveform is empty");
        (self.times[i], self.value(i))
    }

    /// All samples after the first, flattened: the values a window iterates
    /// on (the first sample is the fixed start of the window).
    pub fn interior(&self) -> &[f64] {
        &self.values[self.ncomp.min(self.values.len())..]
    }

    /// Overwrite every sample after the first, keeping the times.
    ///
    /// # Panics
    /// Panics if `v` does not have `(len() - 1) * ncomp` entries.
    pub fn set_interior(&mut self, v: &[f64]) {
        let start = self.ncomp;
        assert_eq!(
            v.len(),
            self.values.len() - start,
            "interior length mismatch"
        );
        self.values[start..].copy_from_slice(v);
    }

    /// The value at `t`. Outside the sampled range the nearest end sample is
    /// returned.
    ///
    /// # Panics
    /// Panics if the waveform is empty.
    pub fn sample(&self, t: f64, interp: Interpolation) -> Vec<f64> {
        let mut out = vec![0.0; self.ncomp];
        self.sample_into(t, interp, &mut out);
        out
    }

    /// [`sample`](Self::sample) into a caller buffer.
    pub fn sample_into(&self, t: f64, interp: Interpolation, out: &mut [f64]) {
        assert!(!self.is_empty(), "sampling an empty waveform");
        assert_eq!(
            out.len(),
            self.ncomp,
            "output has the wrong number of components"
        );
        // First sample at or after t.
        let hi = self.times.partition_point(|&s| s < t);
        if hi == 0 || hi == self.len() {
            let i = hi.min(self.len() - 1);
            out.copy_from_slice(self.value(i));
            return;
        }
        match interp {
            Interpolation::Constant => out.copy_from_slice(self.value(hi)),
            Interpolation::Linear => {
                let (t0, t1) = (self.times[hi - 1], self.times[hi]);
                let s = (t - t0) / (t1 - t0);
                let (a, b) = (self.value(hi - 1), self.value(hi));
                for k in 0..self.ncomp {
                    out[k] = a[k] + s * (b[k] - a[k]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_reproduces_linear_data_and_clamps_outside() {
        let mut w = Waveform::new(2);
        w.start(0.0, &[0.0, 1.0]);
        w.push(0.3, &[0.6, 1.0]);
        w.push(1.0, &[2.0, 1.0]);
        for t in [0.0, 0.1, 0.3, 0.55, 1.0] {
            let v = w.sample(t, Interpolation::Linear);
            assert!(
                (v[0] - 2.0 * t).abs() < 1e-14 && v[1] == 1.0,
                "t={t}: {v:?}"
            );
        }
        assert_eq!(w.sample(-1.0, Interpolation::Linear), vec![0.0, 1.0]);
        assert_eq!(w.sample(2.0, Interpolation::Linear), vec![2.0, 1.0]);
        assert_eq!(w.sample(0.1, Interpolation::Constant), vec![0.6, 1.0]);
    }

    #[test]
    fn interior_round_trips_and_restart_keeps_the_end() {
        let mut w = Waveform::new(1);
        w.start(0.0, &[1.0]);
        w.push(0.5, &[2.0]);
        w.push(1.0, &[3.0]);
        assert_eq!(w.interior(), &[2.0, 3.0]);
        w.set_interior(&[5.0, 7.0]);
        assert_eq!(w.end(), (1.0, &[7.0][..]));
        w.restart_from_end();
        assert_eq!((w.len(), w.times()[0], w.value(0)[0]), (1, 1.0, 7.0));
        assert!(w.interior().is_empty());
    }

    #[test]
    #[should_panic(expected = "times must increase")]
    fn times_must_increase() {
        let mut w = Waveform::new(1);
        w.start(1.0, &[0.0]);
        w.push(1.0, &[0.0]);
    }
}
