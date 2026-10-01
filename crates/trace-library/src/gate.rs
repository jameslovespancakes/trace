//! Precision gate: `assets/library/gate.json`. Derived facts on a
//! server-resolved callee are `inferred` only for languages whose gate passed (precision >=
//! the threshold on the labelled sample), else `possible`. A language without a measured
//! sample has `passed: false`. `bridges` holds the bridge gate per language (§1.15).
//!
//! The labelled sample is the lab's `labels.json` ([`LabelFile`], written by
//! `codepath-lab/scripts/libderive/export_labels.py`); [`judge`] scores one label against
//! the derived effects, [`Score`] adds them up per language (the `derive_gate` example
//! derives the labelled functions and writes the result into gate.json).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use trace_core::Language;

use crate::derive::FunctionSummary;
use crate::languages;
use crate::model::{ArgSel, LibraryError};

const BUILTIN_GATE: &str = include_str!("../../../assets/library/gate.json");

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateResult {
    pub precision: f64,
    pub facts: u32,
    pub sample: String,
    pub passed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BridgeGate {
    pub passed: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gate {
    pub schema: u32,
    pub threshold: f64,
    pub languages: BTreeMap<Language, GateResult>,
    pub bridges: BTreeMap<Language, BridgeGate>,
}

impl Gate {
    pub fn load_builtin() -> Result<Gate, LibraryError> {
        serde_json::from_str(BUILTIN_GATE).map_err(|e| LibraryError::Gate(e.to_string()))
    }

    /// Whether derived library facts of `language` count as inferred.
    pub fn passed(&self, language: Language) -> bool {
        self.languages
            .get(&languages::table_language(language))
            .is_some_and(|g| g.passed)
    }

    /// Whether crossings built from derived channel effects of `language` count as inferred.
    pub fn bridges_passed(&self, language: Language) -> bool {
        self.bridges
            .get(&languages::table_language(language))
            .is_some_and(|g| g.passed)
    }
}

// ---------------------------------------------------------------------------------------------
// Labelled sample

/// Effects that run the argument: a derived one of these on a parameter is a derived fact.
pub const RUNS: [&str; 5] = ["calls", "stored_then_called", "wraps", "property", "partial"];

/// The lab's label file (`labels.json`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LabelFile {
    #[serde(default)]
    pub schema: u32,
    /// Language name (`python`, `javascript`, `go`, ...) -> its labels.
    #[serde(default)]
    pub languages: BTreeMap<String, LabelBlock>,
}

/// The labels of one language.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct LabelBlock {
    /// Effects that must be derived where the library source exists.
    #[serde(default)]
    pub known: Vec<Label>,
    /// Effects that must not be derived (data parameters).
    #[serde(default)]
    pub negatives: Vec<Label>,
    /// Labels added by reading the installed library source (`label` says true / false).
    #[serde(default)]
    pub hand_checked: Vec<Label>,
}

/// Which list of a [`LabelBlock`] a label comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    Known,
    Negative,
    HandChecked,
}

impl Part {
    pub const fn as_str(self) -> &'static str {
        match self {
            Part::Known => "known",
            Part::Negative => "negative",
            Part::HandChecked => "hand_checked",
        }
    }
}

/// One label. Located by `module` + `name` (+ `method`: a method of any class of the module),
/// by `api` (library-qualified symbol: the longest module prefix names the file), or by
/// `file` (relative to a library directory / the package root) + `api`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Label {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// A method name on an unknown receiver class of `module`.
    #[serde(default)]
    pub method: bool,
    #[serde(default)]
    pub param: Option<String>,
    /// Argument selector (trace's `ArgSel` JSON: `{"Pos": 0}`, `{"Kw": "x"}`,
    /// `{"PosOrKw": [1, "target"]}`).
    #[serde(default)]
    pub arg: Option<ArgSel>,
    #[serde(default)]
    pub effect: Option<String>,
    /// Derived effects that count as the labelled one.
    #[serde(default)]
    pub accept: Vec<String>,
    /// Effects that must not be derived (negatives).
    #[serde(default)]
    pub forbidden: Vec<String>,
    /// Compiled code without source: answered by the native table, never by derivation.
    #[serde(default)]
    pub native: bool,
    /// `"true"` / `"false"` (hand-checked labels; known = true, negatives = false).
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    /// `<package>@<version>` of hand-checked labels.
    #[serde(default)]
    pub library: Option<String>,
}

impl Label {
    /// Whether the label says the library runs the argument.
    pub fn positive(&self, part: Part) -> bool {
        match self.label.as_deref() {
            Some("false") => false,
            Some("true") => true,
            _ => part != Part::Negative,
        }
    }

    /// Effects matching a positive label (`accept`, else `effect`).
    pub fn expected(&self) -> Vec<&str> {
        if self.accept.is_empty() {
            self.effect.as_deref().into_iter().collect()
        } else {
            self.accept.iter().map(String::as_str).collect()
        }
    }

    /// Effects a negative label forbids (`forbidden`, else `accept`, else every running
    /// effect).
    pub fn forbidden_effects(&self) -> Vec<&str> {
        if !self.forbidden.is_empty() {
            self.forbidden.iter().map(String::as_str).collect()
        } else if !self.accept.is_empty() {
            self.accept.iter().map(String::as_str).collect()
        } else {
            RUNS.to_vec()
        }
    }

    /// A readable identifier.
    pub fn describe(&self) -> String {
        if !self.id.is_empty() {
            return self.id.clone();
        }
        let what = if self.api.is_empty() {
            format!("{}.{}", self.module.as_deref().unwrap_or("?"), self.name.as_deref().unwrap_or("?"))
        } else {
            self.api.clone()
        };
        format!("{what}:{}", self.param.as_deref().unwrap_or("?"))
    }
}

impl LabelFile {
    pub fn parse(text: &str) -> Result<LabelFile, LibraryError> {
        serde_json::from_str(text).map_err(|e| LibraryError::Gate(format!("labels: {e}")))
    }

    /// Every label of a language with its part (known, negatives, hand-checked).
    pub fn labels(&self, language: &str) -> Vec<(Part, &Label)> {
        let Some(block) = self.languages.get(language) else {
            return Vec::new();
        };
        let mut out: Vec<(Part, &Label)> = Vec::new();
        out.extend(block.known.iter().map(|l| (Part::Known, l)));
        out.extend(block.negatives.iter().map(|l| (Part::Negative, l)));
        out.extend(block.hand_checked.iter().map(|l| (Part::HandChecked, l)));
        out
    }
}

/// Position a selector names (`Pos`, `PosOrKw`, the first index of `Rest`).
fn position(sel: &ArgSel) -> Option<u32> {
    match sel {
        ArgSel::Pos(i) | ArgSel::PosOrKw(i, _) | ArgSel::Rest(i) => Some(*i),
        _ => None,
    }
}

/// Parameter name a selector names (`Kw`, `PosOrKw`).
fn name(sel: &ArgSel) -> Option<&str> {
    match sel {
        ArgSel::Kw(n) | ArgSel::PosOrKw(_, n) => Some(n),
        _ => None,
    }
}

/// Effect names a summary derived on the labelled argument (`arg` selector and / or the
/// parameter name): by position, by name, a variadic `Rest(i)` covering the position, or
/// the block.
pub fn effects_at(summary: &FunctionSummary, arg: Option<&ArgSel>, param: Option<&str>) -> BTreeSet<String> {
    let want_pos = arg.and_then(position);
    let want_name = arg.and_then(name).or(param);
    let mut out = BTreeSet::new();
    for (sel, effects) in &summary.params {
        let by_pos = match (sel, want_pos) {
            (ArgSel::Rest(i), Some(p)) => p >= *i,
            (_, Some(p)) => position(sel) == Some(p),
            _ => false,
        };
        let by_name = want_name.is_some() && name(sel) == want_name;
        if by_pos || by_name {
            out.extend(effects.iter().map(|e| e.name().to_string()));
        }
    }
    out
}

/// How one label scores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Compiled code: the native table answers it (not a derivation label).
    Native,
    /// The labelled function was not found in the library source given.
    NotFound,
    /// A positive label expecting only non-running effects (`iterates`): reported, not counted.
    NotCounted,
    /// Positive label, nothing running derived (a miss: recall, not precision).
    Miss,
    /// Positive label, a running effect derived: a correct derived fact.
    Correct,
    /// Negative label, a forbidden running effect derived: a wrong derived fact.
    Wrong,
    /// Negative label, a forbidden non-running effect derived (`iterates` on data): reported.
    WrongOther,
    /// Negative label, nothing forbidden derived.
    Clean,
}

/// Score one label against the effects derived on its argument (`None`: function not found).
pub fn judge(label: &Label, part: Part, derived: Option<&BTreeSet<String>>) -> Verdict {
    if label.native {
        return Verdict::Native;
    }
    let Some(derived) = derived else {
        return Verdict::NotFound;
    };
    let runs = derived.iter().any(|e| RUNS.contains(&e.as_str()));
    if label.positive(part) {
        if !label.expected().iter().any(|e| RUNS.contains(e)) {
            return Verdict::NotCounted;
        }
        return if runs { Verdict::Correct } else { Verdict::Miss };
    }
    let forbidden = label.forbidden_effects();
    let hit: Vec<&String> = derived.iter().filter(|e| forbidden.contains(&e.as_str())).collect();
    if hit.iter().any(|e| RUNS.contains(&e.as_str())) {
        Verdict::Wrong
    } else if !hit.is_empty() {
        Verdict::WrongOther
    } else {
        Verdict::Clean
    }
}

/// Scores of one language: precision = correct / (correct + wrong) derived facts.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Score {
    pub labels: u32,
    pub native: u32,
    pub correct: u32,
    pub wrong: Vec<String>,
    pub wrong_other: Vec<String>,
    pub misses: Vec<String>,
    pub not_found: Vec<String>,
    pub not_counted: Vec<String>,
    pub clean: u32,
}

impl Score {
    pub fn add(&mut self, label: &Label, part: Part, verdict: Verdict, note: &str) {
        self.labels += 1;
        let id = format!("{} [{}]{note}", label.describe(), part.as_str());
        match verdict {
            Verdict::Native => self.native += 1,
            Verdict::NotFound => self.not_found.push(id),
            Verdict::NotCounted => self.not_counted.push(id),
            Verdict::Miss => self.misses.push(id),
            Verdict::Correct => self.correct += 1,
            Verdict::Wrong => self.wrong.push(id),
            Verdict::WrongOther => self.wrong_other.push(id),
            Verdict::Clean => self.clean += 1,
        }
    }

    /// Derived facts on the sample.
    pub fn facts(&self) -> u32 {
        self.correct + self.wrong.len() as u32
    }

    pub fn precision(&self) -> f64 {
        match self.facts() {
            0 => 0.0,
            n => f64::from(self.correct) / f64::from(n),
        }
    }

    /// Whether the language passes: facts derived and precision at least `threshold`.
    pub fn passed(&self, threshold: f64) -> bool {
        self.facts() > 0 && self.precision() >= threshold
    }
}

#[cfg(test)]
#[path = "../tests/unit/gate.rs"]
mod tests;
