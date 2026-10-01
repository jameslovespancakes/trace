//! The walk of the repository that records build files only (bounded).

use super::*;

#[derive(Default)]
pub(super) struct Walk {
    /// Relative paths ('/'-separated) of build files ([`BUILD_FILE_NAMES`]).
    pub(super) files: Vec<String>,
    /// Modules with `src/main/AndroidManifest.xml`.
    pub(super) android_manifest: Vec<String>,
    /// Modules with `.proto` sources under `src/<set>/proto` or `src/<set>/protobuf` of a main
    /// (non-test) source set.
    pub(super) proto: Vec<String>,
    /// Modules with `.proto` sources only needed by a test source set (`src/test*/proto[buf]`):
    /// their classes are generated into the test output (`target/generated-test-sources`).
    pub(super) proto_test: Vec<String>,
}

impl Walk {
    pub(super) fn named(&self, names: &[&str]) -> Vec<&str> {
        self.files
            .iter()
            .filter(|f| names.contains(&relpath::file_name(f)))
            .map(String::as_str)
            .collect()
    }
}

/// The module directory in front of a `src` segment ("a/b/src/main" -> "a/b").
pub(super) fn module_before_src(rel_dir: &str) -> Option<String> {
    let segs: Vec<&str> = rel_dir.split('/').collect();
    let i = segs.iter().rposition(|s| *s == "src")?;
    Some(segs[..i].join("/"))
}

pub(super) fn walk(cx: &DetectContext<'_>) -> Walk {
    let mut out = Walk::default();
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(cx.root.to_path_buf(), String::new(), 0)];
    let mut seen = 0usize;
    'outer: while let Some((dir, rel, depth)) = stack.pop() {
        for (name, path) in entries(&dir) {
            seen += 1;
            if seen > MAX_WALK_ENTRIES {
                break 'outer;
            }
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let ft = meta.file_type();
            if ft.is_symlink() {
                continue;
            }
            let child = relpath::join(&rel, &name);
            if ft.is_dir() {
                if depth + 1 > MAX_WALK_DEPTH
                    || name.starts_with('.')
                    || SKIP_DIRS.contains(&name.as_str())
                    || !cx.allowed(&path)
                {
                    continue;
                }
                stack.push((path, child, depth + 1));
            } else if ft.is_file() {
                if BUILD_FILE_NAMES.contains(&name.as_str()) {
                    if out.files.len() < MAX_BUILD_FILES {
                        out.files.push(child);
                    }
                } else if name == "AndroidManifest.xml" && (rel == "src/main" || rel.ends_with("/src/main")) {
                    if let Some(m) = module_before_src(&rel) {
                        out.android_manifest.push(m);
                    }
                } else if name.ends_with(".proto") {
                    let segs: Vec<&str> = rel.split('/').collect();
                    // Both conventions of the protobuf build plugins: `src/<set>/proto`
                    // and `src/<set>/protobuf`.
                    if let Some(i) = segs.iter().enumerate().position(|(i, s)| {
                        *s == "src" && matches!(segs.get(i + 2), Some(&"proto") | Some(&"protobuf"))
                    }) {
                        let module = segs[..i].join("/");
                        if segs.get(i + 1).is_some_and(|set| set.starts_with("test")) {
                            out.proto_test.push(module);
                        } else {
                            out.proto.push(module);
                        }
                    }
                }
            }
        }
    }
    for list in [&mut out.files, &mut out.android_manifest, &mut out.proto, &mut out.proto_test] {
        list.sort();
        list.dedup();
    }
    out
}

/// The outermost of `dirs` (no other entry is a proper ancestor).
pub(super) fn outermost(dirs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = dirs
        .iter()
        .filter(|d| !dirs.iter().any(|o| o != *d && relpath::within(d, o)))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}
