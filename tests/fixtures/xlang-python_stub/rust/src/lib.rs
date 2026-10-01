// Fixture (P5, python_stub): compiled module `_engine` described by py/pkg/_engine.pyi.
use pyo3::prelude::*;

#[pyclass]
struct Engine {
    running: bool,
}

#[pymethods]
impl Engine {
    #[new]
    fn create() -> Self {
        Engine { running: false }
    }

    fn start(&mut self) {
        self.running = true;
    }
}

#[pyfunction]
fn version() -> String {
    "1".into()
}

#[pymodule]
fn _engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}
