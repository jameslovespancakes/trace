// Fixture (P5, pyo3): a second extension module `_other` (makes `shared` ambiguous).
use pyo3::prelude::*;

#[pyfunction]
fn shared() -> i64 {
    3
}

#[pyfunction]
fn other_only() -> i64 {
    4
}

#[pymodule]
fn _other(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(other_only, m)?)?;
    Ok(())
}
