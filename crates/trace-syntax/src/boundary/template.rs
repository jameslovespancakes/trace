//! String templates: the public [`Tpl`] form and the internal evaluation form (`RawTpl`).

/// Argument selector as far as syntax can see it (trace-bridge maps `trace_library::ArgSel`
/// onto it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgRef {
    Pos(u32),
    Kw(String),
    PosOrKw(u32, String),
    Receiver,
    Last,
    /// Property `field` of the object / dict / hash literal passed at position `.0`
    /// (`fetch(url, {method: "POST"})`).
    Field(u32, String),
}

/// String template of an expression: literals, concatenation, f-strings / template literals,
/// same-file constants, simple imported constants (resolved by the caller through `constant`).
///
/// Parts keep their order: a placeholder inside a template string (`${id}`, an f-string field)
/// is `Hole("{}")`; a part whose value is unknown (a variable, a call result, a leading
/// interpolation such as an unknown base URL) is `Hole("")` and sets `dynamic`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Tpl {
    pub parts: Vec<TplPart>,
    /// A non-literal part the template could not evaluate.
    pub dynamic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TplPart {
    Lit(String),
    /// `"{}"`: a placeholder (one value); `""`: an unknown part (any text).
    Hole(String),
}

/// Name of a placeholder hole in [`TplPart::Hole`].
pub const PLACEHOLDER: &str = "{}";

impl Tpl {
    /// A fully literal template.
    pub fn literal(text: &str) -> Tpl {
        Tpl {
            parts: vec![TplPart::Lit(text.to_string())],
            dynamic: false,
        }
    }

    /// The text when the template is one literal without placeholders or unknown parts.
    pub fn plain(&self) -> Option<String> {
        let mut s = String::new();
        for p in &self.parts {
            match p {
                TplPart::Lit(t) => s.push_str(t),
                TplPart::Hole(_) => return None,
            }
        }
        (!self.dynamic).then_some(s)
    }

    /// Whether the template has any literal text.
    pub fn has_text(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, TplPart::Lit(s) if !s.is_empty()))
    }
}

impl From<&RawTpl> for Tpl {
    fn from(raw: &RawTpl) -> Tpl {
        let mut out = Tpl::default();
        for p in &raw.parts {
            match p {
                Part::Lit(s) => out.parts.push(TplPart::Lit(s.clone())),
                Part::Hole => out.parts.push(TplPart::Hole(PLACEHOLDER.to_string())),
                Part::Unknown => {
                    out.parts.push(TplPart::Hole(String::new()));
                    out.dynamic = true;
                }
            }
        }
        out
    }
}

impl RawTpl {
    /// Internal form of a public template (see [`Tpl`]).
    pub(super) fn from_public(t: &Tpl) -> RawTpl {
        let mut out = RawTpl::default();
        for p in &t.parts {
            match p {
                TplPart::Lit(s) => out.push_lit(s),
                TplPart::Hole(name) if name.is_empty() => out.parts.push(Part::Unknown),
                TplPart::Hole(_) => out.parts.push(Part::Hole),
            }
        }
        if t.dynamic && !out.parts.iter().any(|p| matches!(p, Part::Unknown)) {
            out.parts.push(Part::Unknown);
        }
        if out.parts.is_empty() {
            out.push_lit("");
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Part {
    Lit(String),
    /// A placeholder inside a template (`${id}`, `{}` f-string field): one path value.
    Hole,
    /// A non-literal expression (unknown base URL, variable, call result).
    Unknown,
}

/// A string value built from literals, placeholders and unknown parts (the internal form of
/// the evaluation; [`Tpl`] is the public form).
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct RawTpl {
    pub(super) parts: Vec<Part>,
}

impl RawTpl {
    pub(super) fn lit(s: &str) -> RawTpl {
        RawTpl {
            parts: vec![Part::Lit(s.to_string())],
        }
    }
    pub(super) fn unknown() -> RawTpl {
        RawTpl {
            parts: vec![Part::Unknown],
        }
    }
    pub(super) fn is_literal(&self) -> bool {
        !self.parts.iter().any(|p| matches!(p, Part::Unknown))
    }
    pub(super) fn has_text(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, Part::Lit(s) if !s.is_empty()))
    }
    /// Fully literal string without placeholders.
    pub(super) fn plain(&self) -> Option<String> {
        let mut s = String::new();
        for p in &self.parts {
            match p {
                Part::Lit(t) => s.push_str(t),
                _ => return None,
            }
        }
        Some(s)
    }
    pub(super) fn push_lit(&mut self, s: &str) {
        if let Some(Part::Lit(last)) = self.parts.last_mut() {
            last.push_str(s);
        } else {
            self.parts.push(Part::Lit(s.to_string()));
        }
    }
    pub(super) fn append(&mut self, other: RawTpl) {
        for p in other.parts {
            match p {
                Part::Lit(s) => self.push_lit(&s),
                other => self.parts.push(other),
            }
        }
    }
    pub(super) fn concat(mut self, other: RawTpl) -> RawTpl {
        self.append(other);
        self
    }
}
