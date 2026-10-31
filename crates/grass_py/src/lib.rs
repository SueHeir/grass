//! Python bindings for grass (module `grass`).
//!
//! Exposes the pieces that make sense without writing Rust: mapping data
//! between discretizations (`grass_map`), interface acceleration and
//! waveforms for coupling Python solvers (`grass_multi`), and reading
//! checkpoints (`grass_io`).

use std::sync::Arc;

mod app;

use grass_map::{
    AdjointPair as RAdjointPair, CellBasis, Density, Discretization, Extensive, Field, Intensive,
    MapBuilder, MapReport, Method, PointBasis, PointCloud as RPointCloud, Sampling, SurfaceDofs,
    TriSurface as RTriSurface, UniformGrid as RUniformGrid,
};
use grass_multi::{
    Interpolation, OuterIteration as ROuterIteration, Relaxation, Waveform as RWaveform,
};
use numpy::{IntoPyArray, PyArrayDyn, PyArrayMethods, PyReadonlyArray2, PyReadonlyArrayDyn};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;

type Disc = Arc<dyn Discretization + Send + Sync>;

/// `numpy.asarray(obj, dtype)`, so any array-like (lists, float32, int32)
/// is accepted.
fn as_array<'py>(obj: &Bound<'py, PyAny>, dtype: &str) -> PyResult<Bound<'py, PyAny>> {
    obj.py()
        .import("numpy")?
        .call_method1("ascontiguousarray", (obj, dtype))
}

fn points3(obj: &Bound<'_, PyAny>, what: &str) -> PyResult<Vec<[f64; 3]>> {
    let arr = as_array(obj, "float64")?;
    let a: PyReadonlyArray2<'_, f64> = arr
        .extract()
        .map_err(|_| PyValueError::new_err(format!("{what} must be a 2-D array")))?;
    let a = a.as_array();
    let d = a.ncols();
    if !(1..=3).contains(&d) {
        return Err(PyValueError::new_err(format!(
            "{what} must have shape (n, 1..3), got (n, {d})"
        )));
    }
    Ok(a.rows()
        .into_iter()
        .map(|r| {
            let mut p = [0.0; 3];
            for (k, v) in r.iter().enumerate() {
                p[k] = *v;
            }
            p
        })
        .collect())
}

fn vec3(v: (f64, f64, f64)) -> [f64; 3] {
    [v.0, v.1, v.2]
}

/// Base class of every discretization: entities with centroids and measures.
#[pyclass(subclass, frozen, name = "Discretization")]
struct PyDisc {
    inner: Disc,
}

#[pymethods]
impl PyDisc {
    fn __len__(&self) -> usize {
        self.inner.len()
    }
    /// Entity centroids, shape (n, 3).
    fn centroids<'py>(&self, py: Python<'py>) -> Bound<'py, PyArrayDyn<f64>> {
        let n = self.inner.len();
        let flat: Vec<f64> = (0..n).flat_map(|i| self.inner.centroid(i)).collect();
        flat.into_pyarray(py).reshape(vec![n, 3]).unwrap()
    }
    /// Entity measures (length, area or volume; 1 for bare points).
    fn measures<'py>(&self, py: Python<'py>) -> Bound<'py, PyArrayDyn<f64>> {
        let m = self.inner.measures();
        let n = m.len();
        m.into_pyarray(py).reshape(vec![n]).unwrap()
    }
    fn __repr__(slf: &Bound<'_, Self>) -> PyResult<String> {
        let name = slf.get_type().name()?;
        let d = slf.get();
        Ok(format!(
            "{name}({} entities, {:?})",
            d.inner.len(),
            d.inner.topo_dim()
        ))
    }
}

fn base(inner: Disc) -> PyClassInitializer<PyDisc> {
    PyClassInitializer::from(PyDisc { inner })
}

/// A set of points: particles, sensors, mesh vertices.
///
/// `PointCloud(points, kernel_radius=None, max_distance=None, measures=None)`.
/// Without `kernel_radius` the nearest point carries a value; with it,
/// values are Shepard-normalized Wendland kernel averages.
#[pyclass(extends = PyDisc, frozen)]
struct PointCloud;

#[pymethods]
impl PointCloud {
    #[new]
    #[pyo3(signature = (points, kernel_radius=None, max_distance=None, measures=None))]
    fn new(
        points: &Bound<'_, PyAny>,
        kernel_radius: Option<f64>,
        max_distance: Option<f64>,
        measures: Option<Vec<f64>>,
    ) -> PyResult<PyClassInitializer<Self>> {
        let mut c = RPointCloud::new(points3(points, "points")?);
        if let Some(r) = kernel_radius {
            c = c.with_basis(PointBasis::Kernel { radius: r });
        }
        if let Some(d) = max_distance {
            c = c.with_max_distance(d);
        }
        if let Some(m) = measures {
            c = c.with_measures(m);
        }
        Ok(base(Arc::new(c)).add_subclass(Self))
    }
}

/// A box of equal cells with data at cell centers.
///
/// `UniformGrid(lo, hi, cells, basis="constant", quadrature_order=2)`;
/// `basis` is "constant" (finite-volume) or "trilinear".
#[pyclass(extends = PyDisc, frozen)]
struct UniformGrid;

#[pymethods]
impl UniformGrid {
    #[new]
    #[pyo3(signature = (lo, hi, cells, basis="constant", quadrature_order=2))]
    fn new(
        lo: (f64, f64, f64),
        hi: (f64, f64, f64),
        cells: (usize, usize, usize),
        basis: &str,
        quadrature_order: usize,
    ) -> PyResult<PyClassInitializer<Self>> {
        let basis = match basis {
            "constant" => CellBasis::Constant,
            "trilinear" => CellBasis::Trilinear,
            b => return Err(PyValueError::new_err(format!("unknown basis {b:?}"))),
        };
        let g = RUniformGrid::new(vec3(lo), vec3(hi), [cells.0, cells.1, cells.2])
            .with_basis(basis)
            .with_quadrature_order(quadrature_order);
        Ok(base(Arc::new(g)).add_subclass(Self))
    }
}

/// A triangulated surface with data at vertices or faces.
///
/// `TriSurface(vertices, triangles, dofs="vertex", tolerance=1e-6)`;
/// points farther than `tolerance` from the surface are not on it.
#[pyclass(extends = PyDisc, frozen)]
struct TriSurface;

#[pymethods]
impl TriSurface {
    #[new]
    #[pyo3(signature = (vertices, triangles, dofs="vertex", tolerance=1e-6))]
    fn new(
        vertices: &Bound<'_, PyAny>,
        triangles: &Bound<'_, PyAny>,
        dofs: &str,
        tolerance: f64,
    ) -> PyResult<PyClassInitializer<Self>> {
        let dofs = match dofs {
            "vertex" => SurfaceDofs::Vertex,
            "face" => SurfaceDofs::Face,
            d => return Err(PyValueError::new_err(format!("unknown dofs {d:?}"))),
        };
        let v = points3(vertices, "vertices")?;
        let tarr = as_array(triangles, "int64")?;
        let t: PyReadonlyArray2<'_, i64> = tarr
            .extract()
            .map_err(|_| PyValueError::new_err("triangles must be a 2-D integer array"))?;
        let t = t.as_array();
        if t.ncols() != 3 {
            return Err(PyValueError::new_err("triangles must have shape (m, 3)"));
        }
        let mut tris = Vec::with_capacity(t.nrows());
        for r in t.rows() {
            let mut tri = [0usize; 3];
            for k in 0..3 {
                let i = r[k];
                if i < 0 || i as usize >= v.len() {
                    return Err(PyValueError::new_err(format!(
                        "triangle vertex {i} out of range"
                    )));
                }
                tri[k] = i as usize;
            }
            tris.push(tri);
        }
        Ok(base(Arc::new(RTriSurface::new(v, tris, dofs, tolerance))).add_subclass(Self))
    }
}

fn disc_of(obj: &Bound<'_, PyAny>) -> PyResult<Disc> {
    obj.downcast::<PyDisc>()
        .map(|d| d.get().inner.clone())
        .map_err(|_| PyValueError::new_err("expected a PointCloud, UniformGrid or TriSurface"))
}

fn method_of(m: &str) -> PyResult<Method> {
    match m {
        "native" => Ok(Method::Native),
        "nearest" => Ok(Method::NearestNeighbor),
        m => Err(PyValueError::new_err(format!("unknown method {m:?}"))),
    }
}

fn sampling_of(s: Option<&str>) -> PyResult<Option<Sampling>> {
    match s {
        None => Ok(None),
        Some("average") => Ok(Some(Sampling::Average)),
        Some("centroid") => Ok(Some(Sampling::Centroid)),
        Some(s) => Err(PyValueError::new_err(format!("unknown sampling {s:?}"))),
    }
}

fn report_dict<'py>(py: Python<'py>, r: &MapReport) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("unmapped", r.unmapped.clone())?;
    d.set_item("partial", r.partial)?;
    d.set_item("max_unity_error", r.max_unity_error)?;
    Ok(d)
}

/// Values as (flat, n, ncomp, was_1d).
fn values_of(
    obj: &Bound<'_, PyAny>,
    n_expected: usize,
    what: &str,
) -> PyResult<(Vec<f64>, usize, bool)> {
    let arr = as_array(obj, "float64")?;
    let v: PyReadonlyArrayDyn<'_, f64> = arr.extract()?;
    let a = v.as_array();
    let (n, ncomp, one_d) = match a.ndim() {
        1 => (a.len(), 1, true),
        2 => (a.shape()[0], a.shape()[1], false),
        d => {
            return Err(PyValueError::new_err(format!(
                "{what}: expected 1-D or 2-D values, got {d}-D"
            )))
        }
    };
    if n != n_expected {
        return Err(PyValueError::new_err(format!(
            "{what}: expected {n_expected} entities, got {n}"
        )));
    }
    Ok((a.iter().copied().collect(), ncomp, one_d))
}

fn to_numpy<'py>(
    py: Python<'py>,
    v: Vec<f64>,
    ncomp: usize,
    one_d: bool,
) -> Bound<'py, PyArrayDyn<f64>> {
    let n = v.len() / ncomp;
    let shape = if one_d { vec![n] } else { vec![n, ncomp] };
    v.into_pyarray(py).reshape(shape).unwrap()
}

fn csr_to_scipy<'py>(py: Python<'py>, m: &grass_map::CsrMatrix) -> PyResult<Bound<'py, PyAny>> {
    let mut indptr = vec![0i64];
    let mut indices = Vec::new();
    let mut data = Vec::new();
    for r in 0..m.nrows() {
        for (c, v) in m.row(r) {
            indices.push(c as i64);
            data.push(v);
        }
        indptr.push(indices.len() as i64);
    }
    let sparse = py.import("scipy.sparse")?;
    let args = ((
        data.into_pyarray(py),
        indices.into_pyarray(py),
        indptr.into_pyarray(py),
    ),);
    let kw = PyDict::new(py);
    kw.set_item("shape", (m.nrows(), m.ncols()))?;
    sparse.call_method("csr_matrix", args, Some(&kw))
}

#[derive(Clone, Copy)]
enum Kind {
    Intensive,
    Density,
    Extensive,
}

/// A map from one discretization to another for one kind of field.
///
/// `Map(src, dst, kind="intensive", method="native", sampling=None)`.
/// `kind` is "intensive" (temperature: sampled, constants preserved),
/// "density" (heat flux per area: sampled) or "extensive" (force, heat:
/// redistributed, totals preserved). Call the map on an array of shape
/// (len(src),) or (len(src), k).
#[pyclass(frozen)]
struct Map {
    op: grass_map::CsrMatrix,
    report: MapReport,
    kind: Kind,
    src_len: usize,
    dst_len: usize,
}

#[pymethods]
impl Map {
    #[new]
    #[pyo3(signature = (src, dst, kind="intensive", method="native", sampling=None))]
    fn new(
        src: &Bound<'_, PyAny>,
        dst: &Bound<'_, PyAny>,
        kind: &str,
        method: &str,
        sampling: Option<&str>,
    ) -> PyResult<Self> {
        let (s, d) = (disc_of(src)?, disc_of(dst)?);
        let mut b = MapBuilder::new(&*s, &*d).method(method_of(method)?);
        if let Some(smp) = sampling_of(sampling)? {
            b = b.sampling(smp);
        }
        let (kind, op, report) = match kind {
            "intensive" => {
                let m = b.build::<Intensive>();
                (Kind::Intensive, m.matrix().clone(), m.report().clone())
            }
            "density" => {
                let m = b.build::<Density>();
                (Kind::Density, m.matrix().clone(), m.report().clone())
            }
            "extensive" => {
                let m = b.build::<Extensive>();
                (Kind::Extensive, m.matrix().clone(), m.report().clone())
            }
            k => return Err(PyValueError::new_err(format!("unknown kind {k:?}"))),
        };
        Ok(Self {
            op,
            report,
            kind,
            src_len: s.len(),
            dst_len: d.len(),
        })
    }

    /// Map `values` (on the source) to the target. Sampled kinds leave
    /// unreached target entities at `fill` (default 0.0).
    #[pyo3(signature = (values, fill=0.0))]
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        values: &Bound<'py, PyAny>,
        fill: f64,
    ) -> PyResult<Bound<'py, PyArrayDyn<f64>>> {
        let (v, ncomp, one_d) = values_of(values, self.src_len, "values")?;
        let init = match self.kind {
            Kind::Extensive => 0.0,
            _ => fill,
        };
        let mut out = vec![init; self.dst_len * ncomp];
        py.allow_threads(|| {
            if matches!(self.kind, Kind::Extensive) {
                out.iter_mut().for_each(|x| *x = 0.0);
            }
            self.op.apply(&v, &mut out, ncomp);
        });
        Ok(to_numpy(py, out, ncomp, one_d))
    }

    /// Coverage diagnostics: `unmapped` (entity indices), `partial` (count)
    /// and `max_unity_error`.
    #[getter]
    fn report<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        report_dict(py, &self.report)
    }

    /// The operator as a `scipy.sparse.csr_matrix` (rows: target).
    fn matrix<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        csr_to_scipy(py, &self.op)
    }

    fn __repr__(&self) -> String {
        let k = match self.kind {
            Kind::Intensive => "intensive",
            Kind::Density => "density",
            Kind::Extensive => "extensive",
        };
        format!(
            "Map({} -> {} entities, {k}, {} nonzeros)",
            self.src_len,
            self.dst_len,
            self.op.nnz()
        )
    }
}

/// One operator for both directions between `a` and `b`, so a two-way
/// exchange balances exactly: `sample` brings intensive data from `b` to
/// `a`; `deposit` sends extensive amounts from `a` to `b` through the
/// transpose. `AdjointPair(a, b, method="native", sampling=None)`.
#[pyclass(frozen)]
struct AdjointPair {
    inner: RAdjointPair,
    a_len: usize,
    b_len: usize,
}

#[pymethods]
impl AdjointPair {
    #[new]
    #[pyo3(signature = (a, b, method="native", sampling=None))]
    fn new(
        a: &Bound<'_, PyAny>,
        b: &Bound<'_, PyAny>,
        method: &str,
        sampling: Option<&str>,
    ) -> PyResult<Self> {
        let (da, db) = (disc_of(a)?, disc_of(b)?);
        let m = method_of(method)?;
        let inner = match sampling_of(sampling)? {
            Some(s) => RAdjointPair::with_sampling(&*da, &*db, m, s),
            None => RAdjointPair::new(&*da, &*db, m),
        };
        Ok(Self {
            inner,
            a_len: da.len(),
            b_len: db.len(),
        })
    }

    /// Intensive values on `b` sampled onto `a`.
    fn sample<'py>(
        &self,
        py: Python<'py>,
        on_b: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyArrayDyn<f64>>> {
        let (v, ncomp, one_d) = values_of(on_b, self.b_len, "on_b")?;
        let src = Field::<Intensive>::new(v, ncomp);
        let mut dst = Field::<Intensive>::zeros(self.a_len, ncomp);
        self.inner.sample(&src, &mut dst);
        Ok(to_numpy(py, dst.values, ncomp, one_d))
    }

    /// Extensive amounts on `a` deposited onto `b` (totals preserved).
    fn deposit<'py>(
        &self,
        py: Python<'py>,
        on_a: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyArrayDyn<f64>>> {
        let (v, ncomp, one_d) = values_of(on_a, self.a_len, "on_a")?;
        let src = Field::<Extensive>::new(v, ncomp);
        let mut dst = Field::<Extensive>::zeros(self.b_len, ncomp);
        self.inner.deposit(&src, &mut dst);
        Ok(to_numpy(py, dst.values, ncomp, one_d))
    }

    #[getter]
    fn report<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        report_dict(py, self.inner.report())
    }

    /// The sampling operator (rows: `a`, columns: `b`) as scipy CSR.
    fn matrix<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        csr_to_scipy(py, self.inner.matrix())
    }
}

// ─── Coupling ───────────────────────────────────────────────────────────────

/// Fixed-point acceleration for a coupling interface.
///
/// `OuterIteration(x0, scheme="iqn-ils", omega0=0.1, reuse=0, filter=1e-6,
/// tol=1e-10, max_iters=50)`; `scheme` is "fixed", "aitken" or "iqn-ils".
/// Loop `while not it.done: it.observe(G(it.input))`; call
/// `begin_window()` between time windows.
#[pyclass]
struct OuterIteration {
    inner: ROuterIteration,
}

#[pymethods]
impl OuterIteration {
    #[new]
    #[pyo3(signature = (x0, scheme="iqn-ils", omega0=0.1, reuse=0, filter=1e-6, tol=1e-10, max_iters=50))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        x0: Vec<f64>,
        scheme: &str,
        omega0: f64,
        reuse: usize,
        filter: f64,
        tol: f64,
        max_iters: u32,
    ) -> PyResult<Self> {
        let relax = match scheme {
            "fixed" => Relaxation::Fixed(omega0),
            "aitken" => Relaxation::Aitken { omega0 },
            "iqn-ils" => Relaxation::IqnIls {
                omega0,
                reuse,
                filter,
            },
            s => return Err(PyValueError::new_err(format!("unknown scheme {s:?}"))),
        };
        Ok(Self {
            inner: ROuterIteration::new(x0, relax, tol, max_iters),
        })
    }

    /// The current iterate (feed it to the solvers).
    #[getter]
    fn input<'py>(&self, py: Python<'py>) -> Bound<'py, PyArrayDyn<f64>> {
        to_numpy(py, self.inner.input().to_vec(), 1, true)
    }

    /// Hand in the solvers' raw output for the current input; returns the next input.
    fn observe<'py>(
        &mut self,
        py: Python<'py>,
        x_tilde: Vec<f64>,
    ) -> PyResult<Bound<'py, PyArrayDyn<f64>>> {
        if x_tilde.len() != self.inner.dim() {
            return Err(PyValueError::new_err(format!(
                "expected {} values, got {}",
                self.inner.dim(),
                x_tilde.len()
            )));
        }
        let next = self.inner.observe(&x_tilde).to_vec();
        Ok(to_numpy(py, next, 1, true))
    }

    /// Start the next time window (keeps IQN-ILS history per `reuse`).
    fn begin_window(&mut self) {
        self.inner.begin_window();
    }

    #[getter]
    fn done(&self) -> bool {
        self.inner.done()
    }
    #[getter]
    fn converged(&self) -> bool {
        self.inner.converged()
    }
    #[getter]
    fn iters(&self) -> u32 {
        self.inner.iters()
    }
    #[getter]
    fn residual_norm(&self) -> f64 {
        self.inner.residual_norm()
    }
}

/// Samples of an `ncomp`-component quantity over a time window.
#[pyclass]
struct Waveform {
    inner: RWaveform,
}

#[pymethods]
impl Waveform {
    #[new]
    #[pyo3(signature = (ncomp=1))]
    fn new(ncomp: usize) -> PyResult<Self> {
        if ncomp == 0 {
            return Err(PyValueError::new_err("ncomp must be positive"));
        }
        Ok(Self {
            inner: RWaveform::new(ncomp),
        })
    }
    /// Discard all samples and start at time `t` with `v`.
    fn start(&mut self, t: f64, v: Vec<f64>) {
        self.inner.start(t, &v);
    }
    /// Append a sample (times must increase).
    fn push(&mut self, t: f64, v: Vec<f64>) -> PyResult<()> {
        if v.len() != self.inner.ncomp() {
            return Err(PyValueError::new_err("wrong number of components"));
        }
        if self.inner.times().last().is_some_and(|&l| t <= l) {
            return Err(PyValueError::new_err("waveform times must increase"));
        }
        self.inner.push(t, &v);
        Ok(())
    }
    /// The value at `t`, "linear" (default) or "constant" (next sample).
    #[pyo3(signature = (t, interpolation="linear"))]
    fn __call__(&self, t: f64, interpolation: &str) -> PyResult<Vec<f64>> {
        if self.inner.is_empty() {
            return Err(PyValueError::new_err("empty waveform"));
        }
        let i = match interpolation {
            "linear" => Interpolation::Linear,
            "constant" => Interpolation::Constant,
            s => {
                return Err(PyValueError::new_err(format!(
                    "unknown interpolation {s:?}"
                )))
            }
        };
        Ok(self.inner.sample(t, i))
    }
    #[getter]
    fn times(&self) -> Vec<f64> {
        self.inner.times().to_vec()
    }
    fn __len__(&self) -> usize {
        self.inner.len()
    }
}

// ─── IO ─────────────────────────────────────────────────────────────────────

/// Read a grass checkpoint's header (verifying every checksum) as a dict:
/// `step`, `time`, `provenance` and `entries`.
#[pyfunction]
fn read_checkpoint<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyAny>> {
    let c = grass_io::Checkpoint::read(std::path::Path::new(path))
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    let text = toml::to_string(&c.header).map_err(|e| PyValueError::new_err(e.to_string()))?;
    py.import("tomllib")?.call_method1("loads", (text,))
}

/// Mapping between discretizations, coupling acceleration and waveforms,
/// and checkpoint reading, from the grass simulation framework.
#[pymodule]
fn grass(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyDisc>()?;
    m.add_class::<PointCloud>()?;
    m.add_class::<UniformGrid>()?;
    m.add_class::<TriSurface>()?;
    m.add_class::<Map>()?;
    m.add_class::<AdjointPair>()?;
    m.add_class::<OuterIteration>()?;
    m.add_class::<Waveform>()?;
    m.add_function(wrap_pyfunction!(read_checkpoint, m)?)?;
    app::register(m)?;
    Ok(())
}
