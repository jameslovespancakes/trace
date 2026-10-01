//! Dev-only extraction audit helper (NEXT.md item 17): prints one JSON object per source
//! file with fact counts (declarations by kind, synthetic scopes, calls, anonymous scopes,
//! imports, references by kind, exports) and, with `--spans`, the byte offsets of every
//! declaration name and call, so `scripts/multilang/extraction_audit.py` can compare them
//! with a tree-sitter ground truth.
//!
//! ```text
//! cargo run --release -p trace-syntax --example dump_facts -- <root> [--languages go,rust] [--spans]
//!     [--gt-def SPEC]... [--gt-call SPEC]...
//! ```
//!
//! `--gt-def` / `--gt-call` add a ground truth computed by walking the *pinned* grammar's
//! syntax tree with plain node-kind selectors, independent of trace's queries and tables
//! (used by the audit where the tree_sitter_language_pack grammar is a different grammar
//! family). Selector language (shared with `scripts/multilang/extraction_audit.py`):
//! `KIND:PATH[?COND[&COND]]` for definitions (reported at the byte offset of the node PATH
//! selects, the declared name) and `KIND[?COND[&COND]]` for calls (reported at the node
//! start). PATH steps are separated by `/`: `a|b` (first existing field), `=kind` (first
//! named child of that kind), `#n` (n-th named child), a trailing `*` repeats a step zero or
//! more times; an empty PATH is the node itself. COND: `field=k1|k2` (the field's node kind),
//! `=kind` (has a named child of that kind), `field~t1|t2` (the field's text), `!` negates,
//! `head~t1|t2` (the node is the first argument of a call whose `target` text is listed).
//!
//! Files are listed by `trace_core::inventory::scan` (the same exclusions as `trace
//! index`: .gitignore, hidden/sensitive files, size limits) and only read. Nothing is written
//! besides stdout; target code is never executed.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};
use trace_core::inventory::{scan, InventoryOptions};
use trace_core::model::SymbolKind;
use trace_core::Language;
use trace_syntax::{extract, SourceInput};

fn usage() -> ! {
    eprintln!("usage: dump_facts <root> [--languages a,b] [--spans]");
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut root: Option<PathBuf> = None;
    let mut languages: Option<Vec<Language>> = None;
    let mut spans = false;
    let mut gt_defs: Vec<gt::Spec> = Vec::new();
    let mut gt_calls: Vec<gt::Spec> = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--spans" => spans = true,
            "--gt-def" | "--gt-call" => {
                let Some(text) = args.next() else { usage() };
                match gt::Spec::parse(&text, arg == "--gt-def") {
                    Some(spec) if arg == "--gt-def" => gt_defs.push(spec),
                    Some(spec) => gt_calls.push(spec),
                    None => {
                        eprintln!("invalid selector {text}");
                        usage()
                    }
                }
            }
            "--languages" => {
                let Some(list) = args.next() else { usage() };
                let parsed: Result<Vec<Language>, String> =
                    list.split(',').filter(|s| !s.is_empty()).map(str::parse).collect();
                match parsed {
                    Ok(l) => languages = Some(l),
                    Err(e) => {
                        eprintln!("{e}");
                        usage()
                    }
                }
            }
            _ if root.is_none() && !arg.starts_with("--") => root = Some(PathBuf::from(arg)),
            _ => usage(),
        }
    }
    let Some(root) = root else { usage() };
    let inventory = match scan(&root, &InventoryOptions::default()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("inventory failed: {e}");
            std::process::exit(3);
        }
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for entry in &inventory.sources {
        let Some(language) = entry.language else {
            continue;
        };
        if languages.as_ref().is_some_and(|l| !l.contains(&language)) {
            continue;
        }
        let Ok(source) = std::fs::read(&entry.abs) else {
            continue;
        };
        let mut line = match extract(SourceInput {
            path: &entry.path,
            language,
            source: &source,
        }) {
            Ok(facts) => file_json(&entry.path, language, source.len(), &facts, spans),
            Err(e) => json!({"path": entry.path, "language": language.as_str(), "error": e.to_string()}),
        };
        if !gt_defs.is_empty() || !gt_calls.is_empty() {
            if let Some((defs, calls)) = gt::walk(language, &source, &gt_defs, &gt_calls) {
                line["gt_defs"] = json!(defs);
                line["gt_calls"] = json!(calls);
            }
        }
        if writeln!(out, "{line}").is_err() {
            break;
        }
    }
}

/// Ground truth over the pinned grammar's tree (module docs).
mod gt {
    use trace_core::Language;
    use tree_sitter::Node;

    #[derive(Debug)]
    struct Step {
        alternatives: Vec<String>,
        repeat: bool,
    }

    #[derive(Debug)]
    struct Cond {
        negate: bool,
        field: String,
        /// `=` kinds or `~` texts.
        values: Vec<String>,
        text: bool,
    }

    #[derive(Debug)]
    pub struct Spec {
        kind: String,
        path: Vec<Step>,
        conds: Vec<Cond>,
    }

    impl Spec {
        pub fn parse(text: &str, def: bool) -> Option<Spec> {
            let (head, conds) = match text.split_once('?') {
                Some((h, c)) => (h, c),
                None => (text, ""),
            };
            let (kind, path) = if def {
                head.split_once(':').unwrap_or((head, ""))
            } else {
                (head, "")
            };
            if kind.is_empty() {
                return None;
            }
            let path = path
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let (s, repeat) = match s.strip_suffix('*') {
                        Some(inner) => (inner, true),
                        None => (s, false),
                    };
                    Step {
                        alternatives: s.split('|').map(str::to_string).collect(),
                        repeat,
                    }
                })
                .collect();
            let conds = conds
                .split('&')
                .filter(|s| !s.is_empty())
                .filter_map(|c| {
                    let (negate, c) = match c.strip_prefix('!') {
                        Some(rest) => (true, rest),
                        None => (false, c),
                    };
                    let (field, values, text) = if let Some((f, v)) = c.split_once('~') {
                        (f, v, true)
                    } else {
                        let (f, v) = c.split_once('=')?;
                        (f, v, false)
                    };
                    Some(Cond {
                        negate,
                        field: field.to_string(),
                        values: values.split('|').map(str::to_string).collect(),
                        text,
                    })
                })
                .collect();
            Some(Spec {
                kind: kind.to_string(),
                path,
                conds,
            })
        }
    }

    fn named(node: Node<'_>) -> Vec<Node<'_>> {
        let mut cursor = node.walk();
        let out: Vec<Node<'_>> = node.named_children(&mut cursor).filter(|c| !c.is_extra()).collect();
        out
    }

    fn one<'t>(node: Node<'t>, step: &str) -> Option<Node<'t>> {
        if let Some(kind) = step.strip_prefix('=') {
            return named(node).into_iter().find(|c| c.kind() == kind);
        }
        if let Some(n) = step.strip_prefix('#') {
            return named(node).get(n.parse::<usize>().ok()?).copied();
        }
        node.child_by_field_name(step)
    }

    fn apply<'t>(node: Node<'t>, path: &[Step]) -> Option<Node<'t>> {
        let mut current = node;
        for step in path {
            let pick = |n: Node<'t>| step.alternatives.iter().find_map(|a| one(n, a));
            if step.repeat {
                let mut guard = 0;
                while let Some(next) = pick(current) {
                    current = next;
                    guard += 1;
                    if guard > 32 {
                        break;
                    }
                }
            } else {
                current = pick(current)?;
            }
        }
        Some(current)
    }

    fn text<'s>(node: Node<'_>, source: &'s [u8]) -> &'s str {
        std::str::from_utf8(&source[node.start_byte()..node.end_byte()])
            .unwrap_or("")
            .trim()
    }

    fn holds(node: Node<'_>, cond: &Cond, source: &[u8]) -> bool {
        let result = if cond.field == "head" && cond.text {
            // First named child of an `arguments` node whose call's `target` is listed.
            node.parent().is_some_and(|args| {
                args.kind() == "arguments"
                    && named(args).first().is_some_and(|f| f.id() == node.id())
                    && args
                        .parent()
                        .and_then(|call| call.child_by_field_name("target"))
                        .is_some_and(|t| cond.values.iter().any(|v| v == text(t, source)))
            })
        } else if cond.field.is_empty() {
            named(node).iter().any(|c| cond.values.iter().any(|v| v == c.kind()))
        } else {
            match node.child_by_field_name(&cond.field) {
                Some(child) if cond.text => cond.values.iter().any(|v| v == text(child, source)),
                Some(child) => cond.values.iter().any(|v| v == child.kind()),
                None => false,
            }
        };
        result != cond.negate
    }

    /// `(definition name offsets, call offsets)` over the pinned grammar's tree.
    pub fn walk(
        language: Language,
        source: &[u8],
        defs: &[Spec],
        calls: &[Spec],
    ) -> Option<(Vec<u32>, Vec<u32>)> {
        let grammar = trace_syntax::grammar(language)?;
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&grammar.ts).ok()?;
        let tree = parser.parse(source, None)?;
        let mut out_defs = Vec::new();
        let mut out_calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            let kind = node.kind();
            if node.is_named() {
                for spec in defs {
                    if spec.kind == kind && spec.conds.iter().all(|c| holds(node, c, source)) {
                        if let Some(name) = apply(node, &spec.path) {
                            out_defs.push(name.start_byte() as u32);
                        }
                    }
                }
                for spec in calls {
                    if spec.kind == kind && spec.conds.iter().all(|c| holds(node, c, source)) {
                        out_calls.push(node.start_byte() as u32);
                    }
                }
            }
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
        out_defs.sort_unstable();
        out_defs.dedup();
        out_calls.sort_unstable();
        out_calls.dedup();
        Some((out_defs, out_calls))
    }
}

fn kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Constructor => "constructor",
        SymbolKind::Class => "class",
        SymbolKind::Interface => "interface",
        SymbolKind::Module => "module",
    }
}

fn file_json(
    path: &str,
    language: Language,
    bytes: usize,
    facts: &trace_core::facts::FileFacts,
    spans: bool,
) -> Value {
    let mut declarations: BTreeMap<&str, u64> = BTreeMap::new();
    let mut synthetic: BTreeMap<&str, u64> = BTreeMap::new();
    let mut defs: Vec<Value> = Vec::new();
    for (i, d) in facts.declarations.iter().enumerate() {
        if facts.is_synthetic(i as u32) {
            let key = match d.name.as_str() {
                "<module>" => "module",
                "<genexpr>" => "genexpr",
                _ => "lambda",
            };
            *synthetic.entry(key).or_insert(0) += 1;
            continue;
        }
        *declarations.entry(kind_name(d.kind)).or_insert(0) += 1;
        if spans {
            defs.push(json!([d.span.start_line, d.name_span.start, kind_name(d.kind), d.qualified_name]));
        }
    }
    let mut references: BTreeMap<&str, u64> = BTreeMap::new();
    for r in &facts.references {
        *references.entry(r.kind.as_str()).or_insert(0) += 1;
    }
    let owned = facts.calls.iter().filter(|c| c.owner.is_some()).count();
    let mut value = json!({
        "path": path,
        "language": language.as_str(),
        "bytes": bytes,
        "error_count": facts.error_count,
        "declarations": declarations,
        "synthetic": synthetic,
        "calls": facts.calls.len(),
        "calls_owned": owned,
        "calls_module": facts.calls.len() - owned,
        "anonymous": facts.anonymous.len(),
        "imports": facts.imports.len(),
        "references": references,
        "exports": facts.exports.len(),
    });
    if spans {
        let calls: Vec<Value> = facts
            .calls
            .iter()
            .map(|c| json!([c.line, c.span.start, c.callee_span.start]))
            .collect();
        value["defs"] = Value::Array(defs);
        value["call_spans"] = Value::Array(calls);
    }
    value
}
