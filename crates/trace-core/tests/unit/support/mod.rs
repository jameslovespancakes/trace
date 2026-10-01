//! Test fixtures live under `<temp>/trace-tests/trace-fixtures-core-*` (never in an
//! inspected repository, never in forbidden roots) and are deleted on drop.

use std::fs;
use std::path::PathBuf;

pub(crate) mod index;

pub(crate) struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    pub(crate) fn new(name: &str) -> Fixture {
        let base = crate::paths::normalize_lexically(&std::env::temp_dir().join("trace-tests"));
        fs::create_dir_all(&base).expect("create artifacts directory");
        let dir = tempfile::Builder::new()
            .prefix(&format!("trace-fixtures-core-{name}-"))
            .tempdir_in(&base)
            .expect("create fixture directory");
        Fixture { dir }
    }

    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub(crate) fn dir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(&p).expect("create fixture subdirectory");
        p
    }

    pub(crate) fn write_bytes(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(&p, bytes).expect("write fixture file");
        p
    }

    pub(crate) fn write(&self, rel: &str, text: &str) -> PathBuf {
        self.write_bytes(rel, text.as_bytes())
    }
}
