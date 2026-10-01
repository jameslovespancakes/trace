//! INI-style text: flat `KEY=VALUE` files and sectioned INI documents (`setup.cfg`).

use std::collections::BTreeMap;

/// `KEY=VALUE` / `KEY="VALUE"` lines (`release`, `pyvenv.cfg`, `go env`, `*.properties`); `#`
/// comments.
pub fn key_values(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        let value = ['"', '\'']
            .iter()
            .find_map(|q| value.strip_prefix(*q).and_then(|v| v.strip_suffix(*q)))
            .unwrap_or(value);
        out.insert(key.to_string(), value.to_string());
    }
    out
}

/// INI sections (setup.cfg): `[section]`, `key = value` / `key: value`, indented
/// continuation lines appended with a newline, `#`/`;` comment lines.
pub fn sections(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut section = String::new();
    let mut key: Option<String> = None;
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let indented = raw.starts_with([' ', '\t']);
        if indented {
            if let Some(k) = &key {
                let entry = out.entry(section.clone()).or_default().entry(k.clone()).or_default();
                entry.push('\n');
                entry.push_str(trimmed);
            }
            continue;
        }
        if let Some(name) = trimmed.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            section = name.trim().to_string();
            key = None;
            continue;
        }
        let split = trimmed.find(['=', ':']).map(|i| (&trimmed[..i], &trimmed[i + 1..]));
        if let Some((k, v)) = split {
            let k = k.trim().to_string();
            out.entry(section.clone())
                .or_default()
                .insert(k.clone(), v.trim().to_string());
            key = Some(k);
        }
    }
    out
}

#[cfg(test)]
#[path = "../../tests/unit/formats/ini.rs"]
mod tests;
