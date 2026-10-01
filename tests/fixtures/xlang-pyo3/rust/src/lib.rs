// Fixture (P5, pyo3): a PyO3 extension module `_native` used from Python.
use pyo3::prelude::*;

/// Unique: registered in `_native` -> proven from `_native.fast_sum(...)`.
#[pyfunction]
fn fast_sum(values: Vec<i64>) -> i64 {
    values.iter().sum()
}

/// Exported under another Python name.
#[pyfunction]
#[pyo3(name = "renamed")]
fn internal_name() -> i64 {
    1
}

/// Ambiguous: `shared` is not registered in any module function, and two extension
/// modules exist, so its module cannot be established -> possible.
#[pyfunction]
fn shared() -> i64 {
    2
}

#[pyclass]
struct Counter {
    n: i64,
}

#[pymethods]
impl Counter {
    #[new]
    fn new() -> Self {
        Counter { n: 0 }
    }

    fn bump(&mut self) -> i64 {
        self.n += 1;
        self.n
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(fast_sum, m)?)?;
    m.add_function(wrap_pyfunction!(internal_name, m)?)?;
    m.add_class::<Counter>()?;
    Ok(())
}
