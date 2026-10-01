//! Test helpers shared by the unit tests of trace-env.

use std::fs;
use std::path::Path;

/// Write `text` to `root/rel`, creating the parent directories.
pub(crate) fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}
