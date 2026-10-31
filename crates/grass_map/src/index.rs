//! Uniform hash grid over bounding boxes, for candidate and nearest queries.

use std::collections::HashMap;

use crate::geom::{dist2, Aabb, Point};

/// A uniform bucket grid. Each item is filed in every bucket its box touches.
#[derive(Debug, Clone)]
pub struct SpatialIndex {
    cell: f64,
    buckets: HashMap<[i64; 3], Vec<usize>>,
    bounds: Option<Aabb>,
}

impl SpatialIndex {
    /// Build an index over `boxes`, choosing a bucket size from the mean box
    /// extent (or from the overall extent for point-like items).
    pub fn build(boxes: &[Aabb]) -> Self {
        let mut bounds: Option<Aabb> = None;
        let mut sum_extent = 0.0;
        for b in boxes {
            sum_extent += b.max_extent();
            match bounds.as_mut() {
                Some(all) => all.merge(b),
                None => bounds = Some(*b),
            }
        }
        let cell = match bounds {
            None => 1.0,
            Some(all) => {
                let mean = sum_extent / boxes.len() as f64;
                // Aim for roughly one item per bucket for point clouds.
                let by_count = all.max_extent() / (boxes.len() as f64).cbrt().max(1.0);
                let c = mean.max(by_count * 0.5);
                if c > 0.0 && c.is_finite() {
                    c
                } else {
                    1.0
                }
            }
        };
        let mut idx = Self {
            cell,
            buckets: HashMap::new(),
            bounds,
        };
        for (i, b) in boxes.iter().enumerate() {
            let (lo, hi) = (idx.key(b.lo), idx.key(b.hi));
            for x in lo[0]..=hi[0] {
                for y in lo[1]..=hi[1] {
                    for z in lo[2]..=hi[2] {
                        idx.buckets.entry([x, y, z]).or_default().push(i);
                    }
                }
            }
        }
        idx
    }

    fn key(&self, p: Point) -> [i64; 3] {
        [
            (p[0] / self.cell).floor() as i64,
            (p[1] / self.cell).floor() as i64,
            (p[2] / self.cell).floor() as i64,
        ]
    }

    /// Append ids of items whose buckets overlap `query` (may contain items
    /// whose own boxes do not overlap; callers test exactly). Sorted, unique.
    pub fn candidates(&self, query: &Aabb, out: &mut Vec<usize>) {
        let start = out.len();
        let (lo, hi) = (self.key(query.lo), self.key(query.hi));
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    if let Some(v) = self.buckets.get(&[x, y, z]) {
                        out.extend_from_slice(v);
                    }
                }
            }
        }
        out[start..].sort_unstable();
        let mut w = start;
        for r in start..out.len() {
            if r == start || out[r] != out[w - 1] {
                out[w] = out[r];
                w += 1;
            }
        }
        out.truncate(w);
    }

    /// The item whose `point` is nearest to `p`, searching outward shell by
    /// shell. Ties go to the lowest id.
    pub fn nearest(&self, p: Point, point: impl Fn(usize) -> Point) -> Option<usize> {
        let all = self.bounds?;
        let k = self.key(p);
        // Distance (in buckets) after which every bucket has been visited.
        let far = {
            let a = self.key(all.lo);
            let b = self.key(all.hi);
            (0..3)
                .map(|d| (k[d] - a[d]).abs().max((k[d] - b[d]).abs()))
                .max()
                .unwrap_or(0)
        };
        let mut best: Option<(f64, usize)> = None;
        for r in 0..=far {
            for x in k[0] - r..=k[0] + r {
                for y in k[1] - r..=k[1] + r {
                    for z in k[2] - r..=k[2] + r {
                        let on_shell =
                            (x - k[0]).abs() == r || (y - k[1]).abs() == r || (z - k[2]).abs() == r;
                        if !on_shell {
                            continue;
                        }
                        if let Some(v) = self.buckets.get(&[x, y, z]) {
                            for &i in v {
                                let d = dist2(p, point(i));
                                if best.is_none_or(|(bd, bi)| d < bd || (d == bd && i < bi)) {
                                    best = Some((d, i));
                                }
                            }
                        }
                    }
                }
            }
            // Every unvisited bucket is at least r * cell away.
            if let Some((bd, _)) = best {
                let reach = r as f64 * self.cell;
                if bd <= reach * reach {
                    break;
                }
            }
        }
        best.map(|(_, i)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_matches_brute_force() {
        let pts: Vec<Point> = (0..200)
            .map(|i| {
                let f = i as f64;
                [
                    (f * 0.37).sin() * 3.0,
                    (f * 0.11).cos() * 2.0,
                    (f * 0.07).sin(),
                ]
            })
            .collect();
        let boxes: Vec<Aabb> = pts.iter().map(|p| Aabb::point(*p)).collect();
        let idx = SpatialIndex::build(&boxes);
        for q in [
            [0.0, 0.0, 0.0],
            [5.0, 5.0, 5.0],
            [-2.9, 1.0, 0.3],
            [1.0, -1.9, -0.9],
        ] {
            let brute = (0..pts.len())
                .min_by(|a, b| dist2(q, pts[*a]).partial_cmp(&dist2(q, pts[*b])).unwrap())
                .unwrap();
            let got = idx.nearest(q, |i| pts[i]).unwrap();
            assert!((dist2(q, pts[got]) - dist2(q, pts[brute])).abs() < 1e-15);
        }
    }
}
