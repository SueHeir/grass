//! Small 3-D geometry helpers shared by the built-in discretizations.

/// A point (or vector) in 3-D space.
pub type Point = [f64; 3];

/// Axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// Lower corner.
    pub lo: Point,
    /// Upper corner.
    pub hi: Point,
}

impl Aabb {
    /// A degenerate box around one point.
    pub fn point(p: Point) -> Self {
        Self { lo: p, hi: p }
    }

    /// The smallest box containing every point in `pts`.
    ///
    /// # Panics
    /// Panics if `pts` is empty.
    pub fn of_points(pts: &[Point]) -> Self {
        let mut b = Self::point(pts[0]);
        for p in &pts[1..] {
            b.include(*p);
        }
        b
    }

    /// Grow the box to contain `p`.
    pub fn include(&mut self, p: Point) {
        for (d, &x) in p.iter().enumerate() {
            self.lo[d] = self.lo[d].min(x);
            self.hi[d] = self.hi[d].max(x);
        }
    }

    /// Grow the box to contain `other`.
    pub fn merge(&mut self, other: &Aabb) {
        self.include(other.lo);
        self.include(other.hi);
    }

    /// The box grown by `r` on every side.
    pub fn inflate(&self, r: f64) -> Self {
        Self {
            lo: [self.lo[0] - r, self.lo[1] - r, self.lo[2] - r],
            hi: [self.hi[0] + r, self.hi[1] + r, self.hi[2] + r],
        }
    }

    /// Whether `p` lies inside the box (boundary inclusive).
    pub fn contains(&self, p: Point) -> bool {
        (0..3).all(|d| p[d] >= self.lo[d] && p[d] <= self.hi[d])
    }

    /// Largest edge length.
    pub fn max_extent(&self) -> f64 {
        (0..3).map(|d| self.hi[d] - self.lo[d]).fold(0.0, f64::max)
    }
}

#[inline]
pub(crate) fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
pub(crate) fn add(a: Point, b: Point) -> Point {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
pub(crate) fn scale(a: Point, s: f64) -> Point {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
pub(crate) fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub(crate) fn cross(a: Point, b: Point) -> Point {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
pub(crate) fn dist2(a: Point, b: Point) -> f64 {
    let d = sub(a, b);
    dot(d, d)
}

/// Area of triangle `abc`.
pub(crate) fn triangle_area(a: Point, b: Point, c: Point) -> f64 {
    let n = cross(sub(b, a), sub(c, a));
    0.5 * dot(n, n).sqrt()
}

/// Closest point on triangle `abc` to `p`, returned as barycentric weights
/// `(wa, wb, wc)` (non-negative, summing to 1) and the squared distance.
///
/// Ericson, *Real-Time Collision Detection*, §5.1.5.
pub(crate) fn closest_on_triangle(p: Point, a: Point, b: Point, c: Point) -> ([f64; 3], f64) {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    let bary = if d1 <= 0.0 && d2 <= 0.0 {
        [1.0, 0.0, 0.0]
    } else {
        let bp = sub(p, b);
        let d3 = dot(ab, bp);
        let d4 = dot(ac, bp);
        if d3 >= 0.0 && d4 <= d3 {
            [0.0, 1.0, 0.0]
        } else {
            let vc = d1 * d4 - d3 * d2;
            if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
                let v = d1 / (d1 - d3);
                [1.0 - v, v, 0.0]
            } else {
                let cp = sub(p, c);
                let d5 = dot(ab, cp);
                let d6 = dot(ac, cp);
                if d6 >= 0.0 && d5 <= d6 {
                    [0.0, 0.0, 1.0]
                } else {
                    let vb = d5 * d2 - d1 * d6;
                    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
                        let w = d2 / (d2 - d6);
                        [1.0 - w, 0.0, w]
                    } else {
                        let va = d3 * d6 - d5 * d4;
                        if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
                            let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
                            [0.0, 1.0 - w, w]
                        } else {
                            let denom = 1.0 / (va + vb + vc);
                            let v = vb * denom;
                            let w = vc * denom;
                            [1.0 - v - w, v, w]
                        }
                    }
                }
            }
        }
    };
    let q = add(add(scale(a, bary[0]), scale(b, bary[1])), scale(c, bary[2]));
    (bary, dist2(p, q))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_point_regions() {
        let (a, b, c) = ([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        // Interior, above the plane.
        let (w, d2) = closest_on_triangle([0.25, 0.25, 0.5], a, b, c);
        assert!((w[0] - 0.5).abs() < 1e-12 && (w[1] - 0.25).abs() < 1e-12);
        assert!((d2 - 0.25).abs() < 1e-12);
        // Beyond vertex b.
        let (w, _) = closest_on_triangle([2.0, -1.0, 0.0], a, b, c);
        assert_eq!(w, [0.0, 1.0, 0.0]);
        // Outside edge bc.
        let (w, d2) = closest_on_triangle([1.0, 1.0, 0.0], a, b, c);
        assert!((w[1] - 0.5).abs() < 1e-12 && (w[2] - 0.5).abs() < 1e-12);
        assert!((d2 - 0.5).abs() < 1e-12);
    }
}
