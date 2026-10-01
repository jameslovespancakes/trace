// Fixture (P5, python_stub): a second crate also building a module named `_engine` and
// registering `version` (another build variant) -> the stub's `version` is ambiguous.
use pyo3::prelude::*;

#[pyfunction]
fn version() -> String {
    "2".into()
}

#[pymodule]
fn _engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}
