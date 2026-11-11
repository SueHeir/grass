//! Compressed-sparse-row matrix used to store mapping operators.

/// A sparse matrix in compressed-sparse-row form.
#[derive(Debug, Clone, PartialEq)]
pub struct CsrMatrix {
    nrows: usize,
    ncols: usize,
    indptr: Vec<usize>,
    indices: Vec<usize>,
    values: Vec<f64>,
}

impl CsrMatrix {
    /// Build from `(row, col, value)` triplets. Duplicate entries are summed;
    /// exact zeros are dropped.
    ///
    /// # Panics
    /// Panics if a row or column index is out of range.
    pub fn from_triplets(nrows: usize, ncols: usize, mut t: Vec<(usize, usize, f64)>) -> Self {
        t.sort_unstable_by_key(|e| (e.0, e.1));
        let mut indptr = vec![0usize; nrows + 1];
        let mut indices = Vec::with_capacity(t.len());
        let mut values: Vec<f64> = Vec::with_capacity(t.len());
        let mut last: Option<(usize, usize)> = None;
        for (r, c, v) in t {
            assert!(
                r < nrows && c < ncols,
                "triplet ({r}, {c}) outside {nrows}x{ncols}"
            );
            if last == Some((r, c)) {
                *values.last_mut().unwrap() += v;
            } else {
                indptr[r + 1] += 1;
                indices.push(c);
                values.push(v);
                last = Some((r, c));
            }
        }
        for r in 0..nrows {
            indptr[r + 1] += indptr[r];
        }
        let mut m = Self {
            nrows,
            ncols,
            indptr,
            indices,
            values,
        };
        m.drop_zeros();
        m
    }

    fn drop_zeros(&mut self) {
        if self.values.iter().all(|v| *v != 0.0) {
            return;
        }
        let mut indptr = vec![0usize; self.nrows + 1];
        let mut indices = Vec::with_capacity(self.indices.len());
        let mut values = Vec::with_capacity(self.values.len());
        for r in 0..self.nrows {
            for k in self.indptr[r]..self.indptr[r + 1] {
                if self.values[k] != 0.0 {
                    indices.push(self.indices[k]);
                    values.push(self.values[k]);
                }
            }
            indptr[r + 1] = indices.len();
        }
        self.indptr = indptr;
        self.indices = indices;
        self.values = values;
    }

    /// Number of rows.
    pub fn nrows(&self) -> usize {
        self.nrows
    }

    /// Number of columns.
    pub fn ncols(&self) -> usize {
        self.ncols
    }

    /// Number of stored nonzeros.
    pub fn nnz(&self) -> usize {
        self.values.len()
    }

    /// The `(column, value)` entries of row `r`.
    pub fn row(&self, r: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
        (self.indptr[r]..self.indptr[r + 1]).map(move |k| (self.indices[k], self.values[k]))
    }

    /// Whether row `r` has no stored entries.
    pub fn row_is_empty(&self, r: usize) -> bool {
        self.indptr[r] == self.indptr[r + 1]
    }

    /// Sum of each row.
    pub fn row_sums(&self) -> Vec<f64> {
        (0..self.nrows)
            .map(|r| self.row(r).map(|(_, v)| v).sum())
            .collect()
    }

    /// Sum of each column.
    pub fn col_sums(&self) -> Vec<f64> {
        let mut s = vec![0.0; self.ncols];
        for (c, v) in self.indices.iter().zip(&self.values) {
            s[*c] += v;
        }
        s
    }

    /// The transposed matrix.
    pub fn transpose(&self) -> Self {
        let mut t = Vec::with_capacity(self.nnz());
        for r in 0..self.nrows {
            for (c, v) in self.row(r) {
                t.push((c, r, v));
            }
        }
        Self::from_triplets(self.ncols, self.nrows, t)
    }

    /// `y = A x` for interleaved vectors with `ncomp` components per entity.
    /// Rows with no entries leave `y` untouched.
    pub fn apply(&self, x: &[f64], y: &mut [f64], ncomp: usize) {
        assert_eq!(x.len(), self.ncols * ncomp, "source length mismatch");
        assert_eq!(y.len(), self.nrows * ncomp, "target length mismatch");
        for r in 0..self.nrows {
            if self.row_is_empty(r) {
                continue;
            }
            for k in 0..ncomp {
                y[r * ncomp + k] = self.row(r).map(|(c, v)| v * x[c * ncomp + k]).sum();
            }
        }
    }

    /// `y = Aᵀ x` for interleaved vectors with `ncomp` components per entity.
    /// Overwrites `y`.
    pub fn apply_transpose(&self, x: &[f64], y: &mut [f64], ncomp: usize) {
        assert_eq!(x.len(), self.nrows * ncomp, "source length mismatch");
        assert_eq!(y.len(), self.ncols * ncomp, "target length mismatch");
        y.iter_mut().for_each(|v| *v = 0.0);
        for r in 0..self.nrows {
            for (c, v) in self.row(r) {
                for k in 0..ncomp {
                    y[c * ncomp + k] += v * x[r * ncomp + k];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triplets_sum_and_transpose() {
        let a = CsrMatrix::from_triplets(
            2,
            3,
            vec![(1, 2, 1.0), (0, 0, 2.0), (1, 2, 3.0), (0, 1, 0.0)],
        );
        assert_eq!(a.nnz(), 2);
        assert_eq!(a.row(1).collect::<Vec<_>>(), vec![(2, 4.0)]);
        let t = a.transpose();
        assert_eq!(t.nrows(), 3);
        assert_eq!(t.row(2).collect::<Vec<_>>(), vec![(1, 4.0)]);
        let x = [1.0, 2.0];
        let mut y1 = [0.0; 3];
        let mut y2 = [0.0; 3];
        a.apply_transpose(&x, &mut y1, 1);
        t.apply(&x, &mut y2, 1);
        assert_eq!(y1, y2);
    }
}
