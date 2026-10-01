//! `cfg(...)` target expressions evaluated against the host triple (structured tokenizer and
//! parser, not a regex).

use super::*;

/// Three-valued truth: predicates trace cannot decide (features, target features) are Unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tri {
    True,
    False,
    Unknown,
}

impl Tri {
    pub(super) fn or(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::True, _) | (_, Tri::True) => Tri::True,
            (Tri::False, Tri::False) => Tri::False,
            _ => Tri::Unknown,
        }
    }
    fn and(self, other: Tri) -> Tri {
        match (self, other) {
            (Tri::False, _) | (_, Tri::False) => Tri::False,
            (Tri::True, Tri::True) => Tri::True,
            _ => Tri::Unknown,
        }
    }
    fn not(self) -> Tri {
        match self {
            Tri::True => Tri::False,
            Tri::False => Tri::True,
            Tri::Unknown => Tri::Unknown,
        }
    }
}

/// The `cfg` facts of a target triple.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostCfg {
    pub triple: String,
    pub values: BTreeMap<&'static str, String>,
    pub names: BTreeSet<&'static str>,
}

impl HostCfg {
    pub(crate) fn from_triple(triple: &str) -> HostCfg {
        let parts: Vec<&str> = triple.split('-').collect();
        let arch = parts.first().copied().unwrap_or_default();
        let vendor = parts.get(1).copied().unwrap_or("unknown");
        let (os_name, env) = if triple.contains("-windows") {
            ("windows", parts.get(3).copied().unwrap_or("msvc"))
        } else if triple.contains("-apple-darwin") {
            ("macos", "")
        } else if triple.contains("-linux") {
            ("linux", parts.get(3).copied().unwrap_or("gnu"))
        } else {
            (parts.get(2).copied().unwrap_or_default(), parts.get(3).copied().unwrap_or_default())
        };
        let family = if os_name == "windows" { "windows" } else { "unix" };
        let arch_cfg = match arch {
            a if a.starts_with("armv7") || a == "arm" => "arm",
            "i686" | "i586" => "x86",
            a if a.starts_with("riscv64") => "riscv64",
            a => a,
        };
        let width = if ["x86", "arm"].contains(&arch_cfg) {
            "32"
        } else {
            "64"
        };
        let mut values = BTreeMap::new();
        values.insert("target_os", os_name.to_string());
        values.insert("target_family", family.to_string());
        values.insert("target_arch", arch_cfg.to_string());
        values.insert("target_env", env.to_string());
        values.insert("target_vendor", vendor.to_string());
        values.insert("target_pointer_width", width.to_string());
        values.insert(
            "target_endian",
            if arch_cfg.contains("s390x") {
                "big"
            } else {
                "little"
            }
            .to_string(),
        );
        let mut names = BTreeSet::new();
        names.insert(family);
        HostCfg {
            triple: triple.to_string(),
            values,
            names,
        }
    }
}

/// A `[target.<spec>]` key: a triple or `cfg(...)`.
pub(crate) fn eval_target(spec: &str, host: &HostCfg) -> Tri {
    let spec = spec.trim();
    match spec.strip_prefix("cfg(").and_then(|s| s.strip_suffix(')')) {
        Some(expr) => {
            let tokens = tokenize(expr);
            let mut pos = 0;
            let value = parse_pred(&tokens, &mut pos, host, 0);
            if pos == tokens.len() {
                value
            } else {
                Tri::Unknown
            }
        }
        None => {
            if spec == host.triple {
                Tri::True
            } else {
                Tri::False
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tok {
    Ident(String),
    Str(String),
    Open,
    Close,
    Comma,
    Eq,
}

pub(super) fn tokenize(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '(' => out.push(Tok::Open),
            ')' => out.push(Tok::Close),
            ',' => out.push(Tok::Comma),
            '=' => out.push(Tok::Eq),
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    s.push(chars[i]);
                    i += 1;
                }
                out.push(Tok::Str(s));
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut s = String::new();
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    s.push(chars[i]);
                    i += 1;
                }
                out.push(Tok::Ident(s));
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

pub(super) fn parse_pred(tokens: &[Tok], pos: &mut usize, host: &HostCfg, depth: usize) -> Tri {
    if depth > 32 {
        return Tri::Unknown;
    }
    let Some(Tok::Ident(name)) = tokens.get(*pos).cloned() else {
        *pos = tokens.len() + 1;
        return Tri::Unknown;
    };
    *pos += 1;
    match tokens.get(*pos) {
        Some(Tok::Open) if matches!(name.as_str(), "all" | "any" | "not") => {
            *pos += 1;
            let mut items = Vec::new();
            while !matches!(tokens.get(*pos), Some(Tok::Close) | None) {
                items.push(parse_pred(tokens, pos, host, depth + 1));
                if matches!(tokens.get(*pos), Some(Tok::Comma)) {
                    *pos += 1;
                }
            }
            *pos += 1; // ')'
            match name.as_str() {
                "all" => items.into_iter().fold(Tri::True, Tri::and),
                "any" => items.into_iter().fold(Tri::False, Tri::or),
                _ => items.first().copied().map(Tri::not).unwrap_or(Tri::Unknown),
            }
        }
        Some(Tok::Eq) => {
            *pos += 1;
            let Some(Tok::Str(value)) = tokens.get(*pos).cloned() else {
                return Tri::Unknown;
            };
            *pos += 1;
            match host.values.iter().find(|(k, _)| **k == name) {
                Some((_, v)) if name == "target_family" => {
                    if *v == value || (value == "unix" && v == "unix") {
                        Tri::True
                    } else {
                        Tri::False
                    }
                }
                Some((_, v)) => {
                    if *v == value {
                        Tri::True
                    } else {
                        Tri::False
                    }
                }
                None => Tri::Unknown,
            }
        }
        _ => match name.as_str() {
            "unix" | "windows" => {
                if host.names.contains(name.as_str()) {
                    Tri::True
                } else {
                    Tri::False
                }
            }
            "test" | "debug_assertions" | "doc" | "miri" => Tri::Unknown,
            _ => Tri::Unknown,
        },
    }
}
