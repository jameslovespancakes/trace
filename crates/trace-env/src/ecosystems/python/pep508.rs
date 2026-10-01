//! PEP 508 requirement lines and environment markers, read by a small tokenizer and a
//! recursive-descent parser (no regex).

use crate::os::{Arch, Os, Platform, Version};

/// A parsed requirement: the distribution name and the marker expression, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub name: String,
    pub marker: Option<Marker>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Marker {
    And(Box<Marker>, Box<Marker>),
    Or(Box<Marker>, Box<Marker>),
    Compare {
        left: Operand,
        op: String,
        right: Operand,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Variable(String),
    Literal(String),
}

/// The values markers are evaluated against (this machine + the project's Python).
pub struct MarkerEnv {
    pub python_version: Option<String>,
    pub sys_platform: &'static str,
    pub platform_system: &'static str,
    pub os_name: &'static str,
    pub platform_machine: String,
}

impl MarkerEnv {
    pub fn new(platform: &Platform, python_version: Option<&str>) -> MarkerEnv {
        let (sys_platform, platform_system, os_name) = match platform.os {
            Os::Windows => ("win32", "Windows", "nt"),
            Os::Linux => ("linux", "Linux", "posix"),
            Os::MacOs => ("darwin", "Darwin", "posix"),
        };
        let platform_machine = match (platform.os, platform.arch) {
            (Os::Windows, Arch::X86_64) => "AMD64".to_string(),
            (Os::Windows, Arch::Aarch64) => "ARM64".to_string(),
            (Os::MacOs, Arch::Aarch64) => "arm64".to_string(),
            _ => platform.arch_name.clone(),
        };
        MarkerEnv {
            python_version: python_version.map(str::to_string),
            sys_platform,
            platform_system,
            os_name,
            platform_machine,
        }
    }

    fn value(&self, variable: &str) -> Option<String> {
        Some(match variable {
            "python_version" | "python_full_version" => self.python_version.clone()?,
            "sys_platform" => self.sys_platform.to_string(),
            "platform_system" => self.platform_system.to_string(),
            "os_name" | "os.name" => self.os_name.to_string(),
            "platform_machine" => self.platform_machine.clone(),
            "implementation_name" => "cpython".to_string(),
            "platform_python_implementation" => "CPython".to_string(),
            // A base requirement is never installed "for an extra".
            "extra" => String::new(),
            _ => return None,
        })
    }
}

/// Parse one requirement line. `None` for URLs, paths and lines without a valid name.
pub fn parse(line: &str) -> Option<Parsed> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(['.', '/', '\\', '-']) {
        return None;
    }
    // A URL is only a requirement in the `name @ url` form (`git+https://...@v1` is not).
    if let Some((scheme_part, _)) = line.split_once("://") {
        if !scheme_part.contains('@') {
            return None;
        }
    }
    let (body, marker_text) = match line.split_once(';') {
        Some((b, m)) => (b, Some(m)),
        None => (line, None),
    };
    let end = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(body.len());
    let name = body[..end].trim_matches(['-', '_', '.']).to_string();
    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    // A drive letter (`C:\...`) or a bare path is not a name.
    let rest = body[end..].trim_start();
    if rest.starts_with(':') || rest.starts_with(['/', '\\']) {
        return None;
    }
    let marker = match marker_text {
        Some(text) if !text.trim().is_empty() => Some(parse_marker(text)?),
        _ => None,
    };
    Some(Parsed { name, marker })
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Word(String),
    Str(String),
    Op(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            out.push(Token::Open);
            i += 1;
        } else if c == ')' {
            out.push(Token::Close);
            i += 1;
        } else if c == '"' || c == '\'' {
            let end = chars[i + 1..].iter().position(|&d| d == c)? + i + 1;
            out.push(Token::Str(chars[i + 1..end].iter().collect()));
            i = end + 1;
        } else if matches!(c, '<' | '>' | '=' | '!' | '~') {
            let mut op = String::new();
            while i < chars.len() && matches!(chars[i], '<' | '>' | '=' | '!' | '~') {
                op.push(chars[i]);
                i += 1;
            }
            out.push(Token::Op(op));
        } else if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            let mut w = String::new();
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                w.push(chars[i]);
                i += 1;
            }
            out.push(Token::Word(w));
        } else {
            return None;
        }
    }
    Some(out)
}

/// Parse a marker expression (`python_version < "3.11" and sys_platform != "win32"`).
pub(crate) fn parse_marker(text: &str) -> Option<Marker> {
    let tokens = tokenize(text)?;
    let mut pos = 0;
    let m = parse_or(&tokens, &mut pos)?;
    (pos == tokens.len()).then_some(m)
}

fn parse_or(t: &[Token], pos: &mut usize) -> Option<Marker> {
    let mut left = parse_and(t, pos)?;
    while matches!(t.get(*pos), Some(Token::Word(w)) if w == "or") {
        *pos += 1;
        let right = parse_and(t, pos)?;
        left = Marker::Or(Box::new(left), Box::new(right));
    }
    Some(left)
}

fn parse_and(t: &[Token], pos: &mut usize) -> Option<Marker> {
    let mut left = parse_atom(t, pos)?;
    while matches!(t.get(*pos), Some(Token::Word(w)) if w == "and") {
        *pos += 1;
        let right = parse_atom(t, pos)?;
        left = Marker::And(Box::new(left), Box::new(right));
    }
    Some(left)
}

fn parse_atom(t: &[Token], pos: &mut usize) -> Option<Marker> {
    if t.get(*pos) == Some(&Token::Open) {
        *pos += 1;
        let m = parse_or(t, pos)?;
        if t.get(*pos) != Some(&Token::Close) {
            return None;
        }
        *pos += 1;
        return Some(m);
    }
    let left = operand(t.get(*pos)?)?;
    *pos += 1;
    let op = match t.get(*pos)? {
        Token::Op(op) => op.clone(),
        Token::Word(w) if w == "in" => "in".to_string(),
        Token::Word(w) if w == "not" => {
            if !matches!(t.get(*pos + 1), Some(Token::Word(x)) if x == "in") {
                return None;
            }
            *pos += 1;
            "not in".to_string()
        }
        _ => return None,
    };
    *pos += 1;
    let right = operand(t.get(*pos)?)?;
    *pos += 1;
    Some(Marker::Compare { left, op, right })
}

fn operand(t: &Token) -> Option<Operand> {
    match t {
        Token::Word(w) => Some(Operand::Variable(w.clone())),
        Token::Str(s) => Some(Operand::Literal(s.clone())),
        _ => None,
    }
}

/// Evaluate a marker. Unknown values (an unknown variable, or the Python version when no
/// file names it) count as "applies", so a requirement is never dropped on a guess.
pub fn evaluate(marker: &Marker, env: &MarkerEnv) -> bool {
    eval(marker, env).unwrap_or(true)
}

fn eval(marker: &Marker, env: &MarkerEnv) -> Option<bool> {
    match marker {
        Marker::And(a, b) => match (eval(a, env), eval(b, env)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        Marker::Or(a, b) => match (eval(a, env), eval(b, env)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
        Marker::Compare { left, op, right } => {
            let version_var = |o: &Operand| matches!(o, Operand::Variable(v) if v == "python_version" || v == "python_full_version");
            let is_version = version_var(left) || version_var(right);
            let value = |o: &Operand| match o {
                Operand::Variable(v) => env.value(v),
                Operand::Literal(s) => Some(s.clone()),
            };
            let (l, r) = (value(left)?, value(right)?);
            compare(&l, op, &r, is_version)
        }
    }
}

fn compare(l: &str, op: &str, r: &str, is_version: bool) -> Option<bool> {
    match op {
        "in" => return Some(r.contains(l)),
        "not in" => return Some(!r.contains(l)),
        _ => {}
    }
    if is_version {
        if let Some(prefix) = r.strip_suffix(".*") {
            let matches = l == prefix || l.starts_with(&format!("{prefix}."));
            return match op {
                "==" => Some(matches),
                "!=" => Some(!matches),
                _ => None,
            };
        }
        let (a, b) = (Version::parse(l)?, Version::parse(r)?);
        let ord = {
            let n = a.parts.len().max(b.parts.len());
            (0..n)
                .map(|i| {
                    a.parts
                        .get(i)
                        .copied()
                        .unwrap_or(0)
                        .cmp(&b.parts.get(i).copied().unwrap_or(0))
                })
                .find(|o| *o != std::cmp::Ordering::Equal)
                .unwrap_or(std::cmp::Ordering::Equal)
        };
        use std::cmp::Ordering::*;
        return Some(match op {
            "==" | "===" => ord == Equal,
            "!=" => ord != Equal,
            "<" => ord == Less,
            "<=" => ord != Greater,
            ">" => ord == Greater,
            ">=" => ord != Less,
            "~=" => {
                // Compatible release: >= b and == b's prefix without the last part.
                let keep = b.parts.len().saturating_sub(1).max(1);
                ord != Less && a.parts.iter().take(keep).eq(b.parts.iter().take(keep))
            }
            _ => return None,
        });
    }
    match op {
        "==" | "===" => Some(l == r),
        "!=" => Some(l != r),
        _ => None,
    }
}
