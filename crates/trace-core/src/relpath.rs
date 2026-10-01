//! Repository-relative path text (`/`-separated, `""` = the repository root): the one
//! implementation of joining, parents, file names and lexical `.` / `..` resolution that every
//! crate uses for index paths.

/// `dir/name` (`name` when `dir` is the root).
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Parent directory (`a/b/c.rs` -> `a/b`, `c.rs` -> `""`).
pub fn parent(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Last component (`a/b/c.rs` -> `c.rs`).
pub fn file_name(rel: &str) -> &str {
    rel.rsplit_once('/').map_or(rel, |(_, name)| name)
}

/// `path` equals `dir` or is below it (`""` contains everything).
pub fn within(path: &str, dir: &str) -> bool {
    dir.is_empty() || path == dir || path.starts_with(&format!("{dir}/"))
}

/// `base` + `rel` with `.` / `..` resolved (`\` accepted as a separator); `None` when it
/// leaves the repository.
pub fn normalize(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// `path` relative to `dir` (`dir` must contain it).
pub fn relative_to(path: &str, dir: &str) -> String {
    if dir.is_empty() {
        path.to_string()
    } else {
        path.strip_prefix(&format!("{dir}/")).unwrap_or(path).to_string()
    }
}

/// Proper ancestors of a directory, nearest first, ending with the root `""`.
pub fn ancestors(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = dir;
    while !cur.is_empty() {
        cur = parent(cur);
        out.push(cur.to_string());
    }
    out
}

/// `a/./b/../c` -> `a/c` (only `/` separates); `None` when it leaves the repository.
pub fn lexical(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// Last component of a path text with either separator (`/usr/bin/python3` -> `python3`,
/// `C:\Tools\node.exe` -> `node.exe`).
pub fn last_component(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[cfg(test)]
#[path = "../tests/unit/relpath.rs"]
mod tests;
