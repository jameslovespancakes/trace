//! Canonical C / C++ targets: an edge or value reference answered with a prototype / forward
//! declaration whose unique definition exists in the index targets that definition.

use super::*;
use crate::languages::Family;

/// Line of a declaration's name: its start line plus the lines of preceding decorators /
/// attributes (the span covers them).
pub(super) fn name_line(s: &Symbol) -> u32 {
    let decorator_lines: u32 = s.decorators.iter().map(|d| d.matches('\n').count() as u32 + 1).sum();
    s.span.start_line + decorator_lines
}

/// C / C++ declarations that take part in the prototype rule: callables and types that are
/// not synthetic and not nested in a callable.
pub(super) fn c_rule_eligible(symbols: &[Symbol], s: &Symbol) -> bool {
    crate::languages::info(s.language).family == Some(Family::C)
        && (s.kind.is_callable() || s.kind.is_type())
        && !s.is_synthetic()
        && !s
            .parent
            .and_then(|p| symbols.get(p.idx()))
            .is_some_and(|p| p.kind.is_callable())
}

/// Owner path of a C/C++ declaration (dotted): its qualified name without the last segment
/// (namespaces and enclosing classes), extended by the out-of-line container when the
/// qualified name does not already end with it (`A` of `void A::f() {}`); empty for free
/// functions of the global namespace.
pub(super) fn c_owner_path(s: &Symbol) -> String {
    let qualified = s.qualified_name.replace("::", ".");
    let mut owner = qualified
        .rsplit_once('.')
        .map(|(o, _)| o.to_string())
        .unwrap_or_default();
    if let Some(c) = s
        .container
        .as_deref()
        .map(|c| c.replace("::", "."))
        .filter(|c| !c.is_empty())
    {
        let covered = owner == c || owner.ends_with(&format!(".{c}"));
        if !covered {
            owner = if owner.is_empty() {
                c
            } else {
                format!("{owner}.{c}")
            };
        }
    }
    owner
}

/// Two owner paths name the same entity: equal, or (both non-empty) one is a segment suffix
/// of the other (`ns.W` and `W` of an out-of-line `void W::f()` whose namespace is implied).
pub(super) fn owners_match(a: &str, b: &str) -> bool {
    a == b
        || (!a.is_empty()
            && !b.is_empty()
            && (a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))))
}

/// C / C++ prototypes and forward declarations (bodiless callable / type declarations,
/// `is_stub`, also in headers) -> their unique definition (language rule: C symbol names are
/// global, C++ has one definition per entity): exactly one C/C++ definition of the same
/// kind (callable / type) with the same name and a matching owner path ([`owners_match`])
/// exists. Several definitions (static functions of several files, overloads) give nothing.
/// Sorted by prototype id. Shared by the link (canonical targets) and the family rule
/// (`rule:c-prototype` `stub_implementation` edges, trace-infer).
pub fn c_prototype_definitions(symbols: &[Symbol]) -> BTreeMap<SymbolId, SymbolId> {
    // name -> (owner path, definition, is a type)
    let mut definitions: HashMap<&str, Vec<(String, SymbolId, bool)>> = HashMap::new();
    let mut any_stub = false;
    for s in symbols {
        if !c_rule_eligible(symbols, s) {
            continue;
        }
        if s.is_stub {
            any_stub = true;
        } else {
            definitions
                .entry(s.name.as_str())
                .or_default()
                .push((c_owner_path(s), s.id, s.kind.is_type()));
        }
    }
    let mut out = BTreeMap::new();
    if !any_stub {
        return out;
    }
    for s in symbols {
        if !s.is_stub || !c_rule_eligible(symbols, s) {
            continue;
        }
        let Some(defs) = definitions.get(s.name.as_str()) else {
            continue;
        };
        let owner = c_owner_path(s);
        let mut matching = defs
            .iter()
            .filter(|(o, d, ty)| *ty == s.kind.is_type() && *d != s.id && owners_match(o, &owner))
            .map(|(_, d, _)| *d);
        if let (Some(only), None) = (matching.next(), matching.next()) {
            out.insert(s.id, only);
        }
    }
    out
}

/// Edge kinds that keep a declaration as their target: family links (a prototype is the
/// declaration an implementation / definition belongs to) and import / re-export statements.
pub(super) fn keeps_declaration(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Implements
            | EdgeKind::Overrides
            | EdgeKind::StubImplementation
            | EdgeKind::Imports
            | EdgeKind::Reexports
            | EdgeKind::Bridge
    )
}

/// Canonical C / C++ targets (module docs): edges and value references into a prototype with
/// a unique definition target the definition.
pub(super) fn canonical_c_targets(symbols: &[Symbol], edges: &mut [Edge], value_refs: &mut [ValueRef]) {
    let definitions = c_prototype_definitions(symbols);
    if definitions.is_empty() {
        return;
    }
    for e in edges.iter_mut() {
        if keeps_declaration(e.kind) {
            continue;
        }
        if let Some(&def) = definitions.get(&e.to) {
            e.to = def;
        }
    }
    for r in value_refs.iter_mut() {
        if let Some(&def) = definitions.get(&r.target) {
            r.target = def;
        }
    }
}

/// What the prototype rule reads of one declaration: name, qualified name, container, kind,
/// stub flag.
pub(super) type CRuleKey = (String, String, Option<String>, SymbolKind, bool);

/// The [`CRuleKey`] of every C / C++ declaration of a record (sorted).
pub(super) fn c_rule_keys(rec: &FileRecord) -> Vec<CRuleKey> {
    if crate::languages::info(rec.language).family != Some(Family::C) {
        return Vec::new();
    }
    let Some(facts) = &rec.facts else {
        return Vec::new();
    };
    let mut out: Vec<CRuleKey> = facts
        .declarations
        .iter()
        .filter(|d| (d.kind.is_callable() || d.kind.is_type()) && !d.name.starts_with('<'))
        .map(|d| (d.name.clone(), d.qualified_name.clone(), d.container.clone(), d.kind, d.is_stub))
        .collect();
    out.sort();
    out
}

/// Whether an incremental link changes a C / C++ declaration named like a prototype of the
/// index or of a new record ([`c_prototype_definitions`] may then map differently).
pub(super) fn c_prototypes_changed(
    prev: &Index,
    input: &AssembleDeltaInput,
    new_order: &[usize],
    removed: &HashSet<&str>,
) -> bool {
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for &ni in new_order {
        let rec = &input.files[ni];
        seen.insert(rec.path.as_str());
        let before = prev
            .file_by_path(&rec.path)
            .map(|id| c_rule_keys(prev.file(id)))
            .unwrap_or_default();
        let after = c_rule_keys(rec);
        if before != after {
            let a: BTreeSet<&CRuleKey> = before.iter().collect();
            let b: BTreeSet<&CRuleKey> = after.iter().collect();
            names.extend(a.symmetric_difference(&b).map(|k| k.0.clone()));
        }
    }
    for path in removed {
        if seen.contains(path) {
            continue;
        }
        if let Some(id) = prev.file_by_path(path) {
            names.extend(c_rule_keys(prev.file(id)).into_iter().map(|k| k.0));
        }
    }
    if names.is_empty() {
        return false;
    }
    let stub_named = |language: crate::languages::Language, is_stub: bool, name: &str| {
        crate::languages::info(language).family == Some(Family::C) && is_stub && names.contains(name)
    };
    prev.symbols
        .iter()
        .any(|s| stub_named(s.language, s.is_stub, &s.name))
        || new_order.iter().any(|&ni| {
            let rec = &input.files[ni];
            rec.facts.as_ref().is_some_and(|f| {
                f.declarations
                    .iter()
                    .any(|d| stub_named(rec.language, d.is_stub, &d.name))
            })
        })
}
