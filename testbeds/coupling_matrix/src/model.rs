//! The test problem: a linear ODE system `u' = A u`, partitioned over roles.
//!
//! Component `i` belongs to one role. Its equation has three parts:
//!
//! * a diagonal decay `d_i` (always negative), treated implicitly;
//! * couplings `O_ij` to other components of the same role, evaluated at the
//!   window start (so a role's ranks must share their state every window,
//!   which exercises the role's own communicator);
//! * couplings `C_ij` to components of *other* roles, only along the edges of
//!   the coupling graph. These are what the coupling layer moves.
//!
//! Over one window a role takes `substeps` backward-Euler steps with the
//! coupling values held constant. Explicit schemes take the neighbours'
//! window-start values (Jacobi) or, from lower-numbered roles, their new
//! values (Gauss-Seidel). Implicit schemes iterate until the neighbours'
//! values are this window's end values, which is a fixed point of an affine
//! map that the serial reference solves exactly.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// chain | star | ring | complete
    pub graph: String,
    /// Components per role.
    pub components: Vec<usize>,
    /// explicit-jacobi | explicit-gs | implicit-jacobi | implicit-gs
    pub scheme: String,
    /// bridge (root-bridge shard exchange) | routed (sparse rank-to-rank)
    #[serde(default = "d_exchange")]
    pub exchange: String,
    /// iqn | aitken | picard (implicit only)
    #[serde(default = "d_accel")]
    pub acceleration: String,
    /// Substeps per window, per role (default 1 each).
    #[serde(default)]
    pub substeps: Vec<usize>,
    pub windows: u64,
    pub dt: f64,
    /// Strength of the cross-role coupling.
    pub coupling: f64,
    pub seed: u64,
    /// Make this role panic at this window (failure-handling test).
    #[serde(default)]
    pub fail_role: Option<String>,
    #[serde(default)]
    pub fail_window: u64,
}

fn d_exchange() -> String {
    "bridge".into()
}
fn d_accel() -> String {
    "iqn".into()
}

impl Case {
    pub fn roles(&self) -> usize {
        self.components.len()
    }
    pub fn substeps(&self, role: usize) -> usize {
        self.substeps.get(role).copied().unwrap_or(1)
    }
    pub fn implicit(&self) -> bool {
        self.scheme.starts_with("implicit")
    }
    pub fn gauss_seidel(&self) -> bool {
        self.scheme.ends_with("-gs")
    }
    pub fn role_name(r: usize) -> String {
        format!("r{r}")
    }
    pub fn relaxation(&self) -> grass_multi::Relaxation {
        use grass_multi::Relaxation;
        match self.acceleration.as_str() {
            "iqn" => Relaxation::IqnIls {
                omega0: 0.5,
                reuse: 4,
                filter: 1e-10,
            },
            "aitken" => Relaxation::Aitken { omega0: 0.5 },
            "picard" => Relaxation::Fixed(1.0),
            a => panic!("acceleration `{a}`"),
        }
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
    fn pick(&mut self, n: usize) -> usize {
        ((self.next() * n as f64) as usize).min(n - 1)
    }
}

pub struct System {
    pub n: usize,
    /// First component of each role (and n at the end).
    pub offsets: Vec<usize>,
    pub role_of: Vec<usize>,
    pub d: Vec<f64>,
    pub intra: Vec<Vec<(usize, f64)>>,
    pub inter: Vec<Vec<(usize, f64)>>,
    pub u0: Vec<f64>,
    /// Role pairs (i < j) that are coupled.
    pub edges: Vec<(usize, usize)>,
}

impl System {
    pub fn build(c: &Case) -> Self {
        let r = c.roles();
        assert!(r >= 2 && c.components.iter().all(|n| *n >= 1));
        let mut offsets = vec![0];
        for n in &c.components {
            offsets.push(offsets.last().unwrap() + n);
        }
        let n = offsets[r];
        let role_of: Vec<usize> = (0..n)
            .map(|i| (0..r).find(|q| i < offsets[q + 1]).unwrap())
            .collect();
        let mut edges: Vec<(usize, usize)> = match c.graph.as_str() {
            "chain" => (0..r - 1).map(|q| (q, q + 1)).collect(),
            "star" => (1..r).map(|q| (0, q)).collect(),
            "ring" => {
                let mut e: Vec<_> = (0..r - 1).map(|q| (q, q + 1)).collect();
                if r > 2 {
                    e.push((0, r - 1));
                }
                e
            }
            "complete" => (0..r)
                .flat_map(|a| (a + 1..r).map(move |b| (a, b)))
                .collect(),
            g => panic!("graph `{g}`"),
        };
        edges.sort();
        let mut rng = Rng(c.seed);
        let d: Vec<f64> = (0..n).map(|_| -1.0 - 2.0 * rng.next()).collect();
        let u0: Vec<f64> = (0..n).map(|_| 2.0 * rng.next() - 1.0).collect();
        let mut intra = vec![Vec::new(); n];
        for i in 0..n {
            let q = role_of[i];
            for j in offsets[q]..offsets[q + 1] {
                if j != i && rng.next() < 0.5 {
                    intra[i].push((j, 0.3 * (2.0 * rng.next() - 1.0)));
                }
            }
        }
        let mut inter = vec![Vec::new(); n];
        for &(a, b) in &edges {
            // a few links per edge, both directions
            let links = 1 + rng.pick(3);
            for _ in 0..links {
                let i = offsets[a] + rng.pick(c.components[a]);
                let j = offsets[b] + rng.pick(c.components[b]);
                let sign = if rng.next() < 0.5 { -1.0 } else { 1.0 };
                inter[i].push((j, sign * c.coupling * (0.5 + 0.5 * rng.next())));
                inter[j].push((i, -sign * c.coupling * (0.5 + 0.5 * rng.next())));
            }
        }
        for row in &mut inter {
            row.sort_by(|x, y| x.0.cmp(&y.0));
        }
        Self {
            n,
            offsets,
            role_of,
            d,
            intra,
            inter,
            u0,
            edges,
        }
    }

    pub fn roles(&self) -> usize {
        self.offsets.len() - 1
    }

    pub fn role_range(&self, r: usize) -> std::ops::Range<usize> {
        self.offsets[r]..self.offsets[r + 1]
    }

    /// Components of role `r` owned by role rank `rank` of `size`.
    pub fn block(&self, r: usize, rank: usize, size: usize) -> std::ops::Range<usize> {
        let base = self.offsets[r];
        let m = self.offsets[r + 1] - base;
        base + rank * m / size..base + (rank + 1) * m / size
    }

    pub fn neighbors(&self, r: usize) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .edges
            .iter()
            .filter_map(|&(a, b)| {
                if a == r {
                    Some(b)
                } else if b == r {
                    Some(a)
                } else {
                    None
                }
            })
            .collect();
        v.sort();
        v
    }

    /// Advance the components `comps` over one window. `start` holds the
    /// window-start value of (at least) every component of their role, `ext`
    /// the coupling value of every component of other roles they reference.
    pub fn step(
        &self,
        comps: std::ops::Range<usize>,
        start: &[f64],
        ext: &[f64],
        dt: f64,
        substeps: usize,
    ) -> Vec<f64> {
        let h = dt / substeps as f64;
        comps
            .map(|i| {
                let mut f = 0.0;
                for &(j, o) in &self.intra[i] {
                    f += o * start[j];
                }
                for &(j, c) in &self.inter[i] {
                    let v = ext[j];
                    assert!(
                        v.is_finite(),
                        "component {i} has no value for coupled component {j}"
                    );
                    f += c * v;
                }
                let mut u = start[i];
                for _ in 0..substeps {
                    u = (u + h * f) / (1.0 - h * self.d[i]);
                }
                u
            })
            .collect()
    }

    /// Components a Gauss-Seidel sweep iterates on: those of roles that feed
    /// a lower-numbered role.
    pub fn gs_owners(&self) -> Vec<usize> {
        (0..self.roles())
            .filter(|&r| self.neighbors(r).iter().any(|&q| q < r))
            .collect()
    }

    /// The serial reference: explicit schemes exactly as the distributed code
    /// computes them; implicit schemes by solving each window's fixed point.
    pub fn reference(&self, c: &Case) -> Vec<f64> {
        let mut u = self.u0.clone();
        for _ in 0..c.windows {
            let start = u.clone();
            u = match c.scheme.as_str() {
                "explicit-jacobi" => self.sweep(c, &start, &start, false),
                "explicit-gs" => self.sweep(c, &start, &start, true),
                "implicit-jacobi" | "implicit-gs" => self.fixed_point(c, &start),
                s => panic!("scheme `{s}`"),
            };
        }
        u
    }

    /// One sweep: every role steps from `start`, coupling to `guess`, or (GS)
    /// to the new values of lower roles.
    pub fn sweep(&self, c: &Case, start: &[f64], guess: &[f64], gs: bool) -> Vec<f64> {
        let mut new = vec![f64::NAN; self.n];
        for r in 0..self.roles() {
            let mut ext = guess.to_vec();
            if gs {
                for q in 0..r {
                    let rr = self.role_range(q);
                    ext[rr.clone()].copy_from_slice(&new[rr]);
                }
            }
            let v = self.step(self.role_range(r), start, &ext, c.dt, c.substeps(r));
            new[self.role_range(r)].copy_from_slice(&v);
        }
        new
    }

    fn fixed_point(&self, c: &Case, start: &[f64]) -> Vec<f64> {
        // F(v) = M v + b with F the Jacobi sweep; solve (I - M) u = b.
        let n = self.n;
        let b = self.sweep(c, start, &vec![0.0; n], false);
        let mut a = vec![vec![0.0; n]; n];
        for j in 0..n {
            let mut e = vec![0.0; n];
            e[j] = 1.0;
            let col = self.sweep(c, start, &e, false);
            for i in 0..n {
                a[i][j] = -(col[i] - b[i]);
            }
            a[j][j] += 1.0;
        }
        solve(a, b)
    }
}

pub fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Vec<f64> {
    let n = b.len();
    for k in 0..n {
        let p = (k..n)
            .max_by(|&x, &y| a[x][k].abs().total_cmp(&a[y][k].abs()))
            .unwrap();
        a.swap(k, p);
        b.swap(k, p);
        for i in k + 1..n {
            let f = a[i][k] / a[k][k];
            for j in k..n {
                a[i][j] -= f * a[k][j];
            }
            b[i] -= f * b[k];
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|j| a[i][j] * x[j]).sum();
        x[i] = (b[i] - s) / a[i][i];
    }
    x
}
