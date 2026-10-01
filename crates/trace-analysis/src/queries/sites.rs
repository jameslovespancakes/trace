//! Query-time site facts shared by `show`, `uses`, `deps`, `path` and `context`:
//!
//! * [`Sites`]: display views of call / use sites ([`trace_syntax::callsite`]: the call on
//!   one line, its conditions, the options they test, the arguments), each file parsed once;
//! * [`carries`]: `argument -> parameter` pairs of a call (callee parameters by position or
//!   keyword; a leading `self` / `cls` / `this` parameter is bound by the receiver);
//! * [`option_tests`]: tests that set an option a site depends on (`router.Debug = true`
//!   for a site guarded by `engine.Debug`): a write of that member name inside a test;
//! * [`entry_points`]: per direct caller, the entry points above it in product code;
//! * [`is_test_code`]: test declarations, scopes nested in them and symbols of test files.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use trace_core::facts::RefKind;
use trace_core::model::{FileId, Location, Symbol, SymbolKind};
use trace_core::source::SourceStore;
use trace_core::{Graph, Index, SymbolId, Tier};
use trace_syntax::{Guard, SiteView};

use crate::cards::{executing_symbol_at, line_at};
use crate::report::{Carry, GuardTest, ImpactChain, SiteInfo};

/// Entry points listed per direct caller.
pub(crate) const ENTRY_POINTS_SHOWN: usize = 6;
/// Option-setting tests listed.
pub(crate) const OPTION_TESTS: usize = 5;
/// Symbols visited per entry-point walk.
const ENTRY_WALK: usize = 5_000;

/// Views of sites, keyed by `(file, byte)`.
#[derive(Default)]
pub struct Sites {
    views: HashMap<(FileId, u32), SiteView>,
}

impl Sites {
    /// Views of every `(file, byte)` position (each file parsed once; unreadable files and
    /// positions outside every node have no view).
    pub fn load(
        index: &Index,
        sources: &SourceStore<'_>,
        points: impl IntoIterator<Item = (FileId, u32)>,
    ) -> Sites {
        let mut by_file: HashMap<FileId, BTreeSet<u32>> = HashMap::new();
        for (file, byte) in points {
            by_file.entry(file).or_default().insert(byte);
        }
        let mut views = HashMap::new();
        for (file, bytes) in by_file {
            let Ok(source) = sources.file(file) else { continue };
            let points: Vec<u32> = bytes.into_iter().collect();
            let language = index.file(file).language;
            for (p, v) in points
                .iter()
                .zip(trace_syntax::site_views(language, &source.bytes, &points))
            {
                if let Some(v) = v {
                    views.insert((file, *p), v);
                }
            }
        }
        Sites { views }
    }

    pub fn view(&self, at: &Location) -> Option<&SiteView> {
        self.views.get(&(at.file, at.bytes.start))
    }

    /// Conditions of the site at `at` (empty without a view).
    pub fn when(&self, at: &Location) -> Vec<String> {
        self.view(at).map(|v| v.when.clone()).unwrap_or_default()
    }

    /// The call text at `at`, else `fallback` (the exact line).
    pub(crate) fn call_or(&self, at: &Location, fallback: &str) -> String {
        match self.view(at) {
            Some(v) if !v.call.is_empty() => v.call.clone(),
            _ => fallback.trim().to_string(),
        }
    }

    /// Display facts of the site at `at` calling `callee` (every argument pair).
    pub fn info(&self, at: &Location, callee: Option<&Symbol>) -> SiteInfo {
        let Some(v) = self.view(at) else { return SiteInfo::default() };
        SiteInfo {
            call: v.call.clone(),
            when: v.when.clone(),
            carries: callee.map(|c| carries(v, c)).unwrap_or_default(),
        }
    }

    /// Options tested by the conditions of the sites at `locations`.
    pub fn guards<'l>(&self, locations: impl IntoIterator<Item = &'l Location>) -> Vec<Guard> {
        let mut out: Vec<Guard> = Vec::new();
        for at in locations {
            for g in self.view(at).map(|v| v.guards.as_slice()).unwrap_or_default() {
                if !out.iter().any(|o| o.name == g.name) {
                    out.push(g.clone());
                }
            }
        }
        out
    }
}

/// `argument -> parameter` pairs of the call in `view` bound to `callee`'s parameters.
pub fn carries(view: &SiteView, callee: &Symbol) -> Vec<Carry> {
    let params = &callee.parameters;
    let offset = usize::from(
        params
            .first()
            .is_some_and(|p| matches!(p.as_str(), "self" | "cls" | "this" | "&self" | "&mut self"))
            && matches!(callee.kind, SymbolKind::Method | SymbolKind::Constructor),
    );
    view.arguments
        .iter()
        .filter_map(|a| {
            let parameter = match (&a.keyword, a.position) {
                (Some(k), _) => params.iter().find(|p| *p == k)?.clone(),
                (None, Some(i)) => params.get(i as usize + offset)?.clone(),
                (None, None) => return None,
            };
            Some(Carry {
                argument: a.text.clone(),
                parameter,
            })
        })
        .collect()
}

/// Test code: a test declaration, a scope nested in one, or a symbol of a test file.
pub fn is_test_code(index: &Index, id: SymbolId) -> bool {
    let s = index.symbol(id);
    let file = index.file(s.file);
    if trace_syntax::is_test_path(&file.path, s.language, &trace_syntax::testing::TestConfig::default())
        || file.facts.as_ref().is_some_and(|f| f.is_test_code())
    {
        return true;
    }
    let mut current = Some(id);
    for _ in 0..64 {
        let Some(c) = current else { break };
        let symbol = index.symbol(c);
        if symbol.is_test {
            return true;
        }
        current = symbol.parent;
    }
    false
}

/// The test containing byte `byte` of `file`: a test declaration (or the declaration a
/// test scope is nested in) or a test block; `(name, line)`.
fn enclosing_test(index: &Index, file: FileId, byte: u32) -> Option<(String, u32)> {
    let record = index.file(file);
    if let Some(block) = record.facts.as_ref().and_then(|f| {
        f.tests
            .iter()
            .filter(|t| t.span.contains(byte))
            .min_by_key(|t| t.span.len())
    }) {
        return Some((block.name.clone(), block.line));
    }
    let mut current = executing_symbol_at(index, file, byte);
    let mut outermost_named = None;
    for _ in 0..64 {
        let Some(c) = current else { break };
        let s = index.symbol(c);
        if s.is_test {
            return Some((s.qualified_name.clone(), s.span.start_line));
        }
        if !s.is_synthetic() {
            outermost_named = Some((s.qualified_name.clone(), s.span.start_line));
        }
        current = s.parent;
    }
    // A named function of a test file (`func TestX` helpers, `describe` bodies).
    let test_file = trace_syntax::is_test_path(
        &record.path,
        record.language,
        &trace_syntax::testing::TestConfig::default(),
    ) || record.facts.as_ref().is_some_and(|f| f.is_test_code());
    if test_file {
        outermost_named
    } else {
        None
    }
}

/// Tests that write one of the `guards` member names (at most `limit`, one row per test,
/// file order), each with the line that sets it.
pub(crate) fn option_tests(
    index: &Index,
    sources: &SourceStore<'_>,
    guards: &[Guard],
    limit: usize,
) -> Vec<GuardTest> {
    if guards.is_empty() {
        return Vec::new();
    }
    let names: HashSet<&str> = guards.iter().map(|g| g.name.as_str()).collect();
    let mut out: Vec<GuardTest> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (i, file) in index.files.iter().enumerate() {
        let Some(facts) = &file.facts else { continue };
        let fid = FileId(i as u32);
        for r in facts
            .references
            .iter()
            .filter(|r| r.kind == RefKind::Write && names.contains(r.name.as_str()))
        {
            let Some((name, line)) = enclosing_test(index, fid, r.span.start) else { continue };
            let test = format!("{}::{name}", file.path);
            if !seen.insert(test.clone()) {
                continue;
            }
            let sets = line_at(sources, fid, r.span.start)
                .map(|(_, _, t)| t.trim().to_string())
                .unwrap_or_default();
            out.push(GuardTest {
                test,
                file: file.path.clone(),
                line,
                sets,
            });
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

/// Per direct caller (product code only), the entry points above it: symbols reached by
/// walking callers upward (at `include`, test code skipped) that nothing in product code
/// calls; the caller itself when nothing calls it.
pub fn entry_points(graph: &Graph<'_>, include: Tier, callers: &[SymbolId]) -> Vec<ImpactChain> {
    let index = graph.index;
    let product = |id: SymbolId| !is_test_code(index, id);
    let mut out = Vec::new();
    let mut done: HashSet<SymbolId> = HashSet::new();
    for &caller in callers {
        if !product(caller) || !done.insert(caller) {
            continue;
        }
        let mut seen: HashSet<SymbolId> = HashSet::from([caller]);
        let mut queue = VecDeque::from([caller]);
        let mut entries: Vec<SymbolId> = Vec::new();
        while let Some(node) = queue.pop_front() {
            let mut has_caller = false;
            for (_, e) in graph.incoming(node, include) {
                if e.from == node || !product(e.from) {
                    continue;
                }
                has_caller = true;
                if seen.len() < ENTRY_WALK && seen.insert(e.from) {
                    queue.push_back(e.from);
                }
            }
            if !has_caller {
                entries.push(node);
            }
        }
        let total = entries.len();
        entries.truncate(ENTRY_POINTS_SHOWN);
        out.push(ImpactChain {
            caller: index.symbol(caller).uid.clone(),
            entry_points: entries.iter().map(|&e| index.symbol(e).uid.clone()).collect(),
            entry_points_total: total,
        });
    }
    out
}

#[cfg(test)]
#[path = "../../tests/unit/queries/sites.rs"]
mod tests;
