//! Override and implementation families by language rule (SPEC §7.10, NEXT.md item 2).
//!
//! [`family_edges`] emits proven `overrides` (method -> concrete base-class method) and
//! `implements` (member -> interface / trait / protocol / abstract / stub method) edges,
//! provider `rule:<name>`, resolution `inheritance_rule`, `at` = the overriding
//! declaration's name span, `from` = the overriding member, `to` = the base member.
//!
//! A base spelling in a class header counts only when it resolves to exactly one class, in
//! this order (never "same directory"):
//! 1. a compiler fact: a semantic value reference (or a reference edge) at an identifier
//!    inside the class header whose target is a type named like the base;
//! 2. exactly one type of that name declared in the same file;
//! 3. exactly one type of that name in the files an import binding the base's root name
//!    resolves to (module path rules of [`crate::narrow::ModuleMap`]);
//! 4. exactly one type of that name in the whole index (same language family).
//!
//! Method lookup per language:
//! * Python (`python-mro`): C3 linearization over the resolved bases (depth-first order when
//!   the linearization is inconsistent); the first class after the class itself that defines
//!   the name is the base.
//! * Out-of-line relations (`FileFacts::impls`, every language that records them: Rust
//!   `impl Trait for Type`, Haskell `instance C T`, ...). The trait / protocol is
//!   resolved first by a compiler fact inside the relation's header (from the relation start
//!   to its first member: a value reference or reference edge whose target is a type named
//!   like the trait, i.e. the server's `definition` of the trait identifier, which works across
//!   crates and packages), then by the unique-name rules 2-4. Members declared inside the
//!   relation block (container = the type) implement the trait's member of the same name;
//!   outside Rust, a member of the conforming type declared anywhere (type body, any
//!   extension of it in any file, attached by the hierarchy's container rule) implements the
//!   requirement of its name too (Rust keeps trait methods to their impl block: an inherent
//!   method of the same name is a different function). Rust: `rust-trait-impl`, only traits.
//! * Haskell (`haskell-inheritance`): `instance C T where m = ...` is an out-of-line relation
//!   (above): the instance members implement the class's method signatures (stubs).
//! * R (`rule:r-s3`): S3 dispatch by naming convention: a function `g.<class>` implements
//!   the generic `g` (a function dispatching with `UseMethod`, a stub in the syntax facts;
//!   [`crate::hierarchy::convention_methods`]).
//! * Go: skipped (structural interfaces have no declared relation).
//! * Everything else with base spellings (`ts-heritage` for JS/TS, `java-inheritance`,
//!   `<language>-inheritance`): breadth-first over the resolved bases; on each path the
//!   nearest definer of the name is the base (`implements` when the definer is an
//!   interface/trait/protocol or the base method is a stub, else `overrides`).
//!
//! Synthetic symbols (`<module>`, `<lambda>`) and constructors named by a language-level
//! constructor kind are never family members. Ambiguous bases produce nothing (dispatch
//! sites keep handling them). A pair a server already proved (`implementation` edges of
//! the same from / to / kind in the index, SPEC 8.5a) is not emitted again.
//!
//! Declarations that are not definitions ([`prototype_edges`], kind `stub_implementation`):
//! * C / C++ (`rule:c-prototype`, resolution `abi_naming_rule`): a prototype or forward
//!   declaration (`is_stub`, also in headers) links to the unique definition with the same
//!   name whose owner path (namespaces, enclosing class, out-of-line container) matches:
//!   equal, or one a segment suffix of the other (`ns.W` / `W` of `void W::f()` written
//!   inside or outside `namespace ns`);
//! * Haskell (`rule:haskell-declaration`): a type signature links to every equation of the
//!   binding with the same name in the same module and scope;
//! * TypeScript (`rule:typescript-declaration` / `rule:tsx-declaration`): a `declare` /
//!   overload signature links to the unique implementation with the same qualified name in
//!   the same module (file); `.d.ts` files are bridged elsewhere.
//!
//! Haskell and TypeScript links use resolution `inheritance_rule`.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use trace_core::facts::{ImplRelation, ImportKind};
use trace_core::languages::Family;
use trace_core::{
    ByteSpan, Edge, EdgeKind, FileId, Index, Language, Location, Provider, Resolution, SymbolId, SymbolKind,
    Tier,
};
use trace_syntax::language_rules::{rules as language_rules, Signatures};

use crate::hierarchy::{base_name, Hierarchy};
use crate::narrow::ModuleMap;

/// Rule name (provider `rule:<name>`) for a language (`LanguageRules::inheritance_rule`).
fn rule_name(language: Language) -> String {
    match language_rules(language).inheritance_rule {
        "" => format!("{}-inheritance", language.as_str()),
        name => name.to_string(),
    }
}

/// Languages whose type relations are structural only (no declared bases): skipped.
fn skipped(language: Language) -> bool {
    language_rules(language).implicit_conformance
}

/// Languages sharing one type namespace for rule 4 (JS and TS files import each other).
fn same_family(a: Language, b: Language) -> bool {
    trace_core::languages::same_module_namespace(a, b)
}

/// Qualifier segments of a dotted / path spelling before `name` (`pkg.mod.Base` ->
/// `[pkg, mod]`, `a_crate::Sink` -> `[a_crate]`); empty when the spelling does
/// not end in `name`.
fn qualifier_of<'s>(spelling: &'s str, name: &str) -> Vec<&'s str> {
    let head = spelling
        .trim()
        .split(['[', '<', '(', '{'])
        .next()
        .unwrap_or("")
        .trim();
    let head = head.rsplit(char::is_whitespace).next().unwrap_or(head);
    let parts: Vec<&str> = head
        .split("::")
        .flat_map(|p| p.split('.'))
        .filter(|p| !p.is_empty())
        .collect();
    match parts.split_last() {
        Some((last, rest)) if *last == name => rest.to_vec(),
        _ => Vec::new(),
    }
}

/// The trait / protocol / interface every out-of-line relation names (module docs), keyed
/// by (file, index into `FileFacts::impls`). Relations whose trait does not resolve
/// uniquely are absent.
pub(crate) fn impl_traits(index: &Index, hierarchy: &Hierarchy) -> HashMap<(FileId, usize), SymbolId> {
    let mut rules = Rules::new(index, hierarchy);
    relation_traits(index, &mut rules)
}

fn relation_traits(index: &Index, rules: &mut Rules<'_>) -> HashMap<(FileId, usize), SymbolId> {
    let mut out = HashMap::new();
    for (fi, file) in index.files.iter().enumerate() {
        if skipped(file.language) {
            continue;
        }
        let Some(facts) = &file.facts else { continue };
        let fid = FileId(fi as u32);
        for (ri, rel) in facts.impls.iter().enumerate() {
            if let Some(t) = rules.resolve_relation_trait(fid, rel) {
                out.insert((fid, ri), t);
            }
        }
    }
    out
}

/// Line of a byte offset within a file (1-based), when known to the caller.
pub(crate) type LineOf<'a> = &'a dyn Fn(FileId, u32) -> Option<u32>;

/// Language groups whose family edges never read each other's declarations: the languages
/// of one [`Family`] (one type namespace: JS/TS, C/C++; the JVM languages: a server's header
/// fact may name a Java base of a Scala class), every other language alone.
fn group_of(language: Language) -> Group {
    match trace_core::languages::info(language).family {
        Some(family) => Group::Family(family),
        None => Group::Language(language),
    }
}

/// A language group of [`group_of`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Group {
    Family(Family),
    Language(Language),
}

/// Incremental family edges (PLAN decision 13, DESIGN §1.14.5): equal to
/// [`family_edges_with`] on the same index. `prev_rule_edges` = the previous family rule
/// edges (every rule provider except the import-path rule), remapped to the new ids.
///
/// Family edges of a language group ([`group_of`]) read only the declarations, relations,
/// header facts and server edges of that group's files (bases resolve within a language
/// family, header facts name types of the group). So the edges of every group none of whose
/// files changed are kept (their `from` member belongs to the group); the groups of changed
/// files are computed again. A removed file's language is no longer known: then every group
/// is computed again.
pub fn family_edges_delta(
    index: &Index,
    hierarchy: &Hierarchy,
    delta: &trace_core::delta::IndexDelta,
    prev_rule_edges: &[Edge],
    line_of: Option<LineOf<'_>>,
) -> Vec<Edge> {
    if delta.full || !delta.removed.is_empty() {
        return family_edges_with(index, hierarchy, line_of);
    }
    let groups: HashSet<Group> = index
        .files
        .iter()
        .filter(|f| delta.file_changed(&f.path))
        .map(|f| group_of(f.language))
        .collect();
    let mut out: Vec<Edge> = prev_rule_edges
        .iter()
        .filter(|e| !groups.contains(&group_of(index.symbol(e.from).language)))
        .cloned()
        .collect();
    if !groups.is_empty() {
        out.extend(family_edges_in(index, hierarchy, line_of, Some(&groups)));
    }
    out
}

/// Proven family edges of the index (module docs). Lines of name spans are approximated
/// from declaration lines (see [`family_edges_with`] for exact lines).
pub fn family_edges(index: &Index) -> Vec<Edge> {
    let hierarchy = Hierarchy::build(index);
    family_edges_with(index, &hierarchy, None)
}

/// [`family_edges`] with a prebuilt hierarchy and an optional exact line resolver.
pub fn family_edges_with(index: &Index, hierarchy: &Hierarchy, line_of: Option<LineOf<'_>>) -> Vec<Edge> {
    family_edges_in(index, hierarchy, line_of, None)
}

/// [`family_edges_with`] restricted to the members (`from`) of the language groups in
/// `groups` (`None`: every group).
fn family_edges_in(
    index: &Index,
    hierarchy: &Hierarchy,
    line_of: Option<LineOf<'_>>,
    groups: Option<&HashSet<Group>>,
) -> Vec<Edge> {
    let wanted = |language: Language| groups.is_none_or(|g| g.contains(&group_of(language)));
    let mut rules = Rules::new(index, hierarchy);
    let mut out: Vec<Edge> = Vec::new();
    // Resolved bases per type (in declaration order).
    let mut bases: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
    for s in &index.symbols {
        if !s.kind.is_type() || s.is_synthetic() || skipped(s.language) || !wanted(s.language) {
            continue;
        }
        let resolved: Vec<SymbolId> = s.bases.iter().filter_map(|b| rules.resolve_base(s.id, b)).collect();
        if !resolved.is_empty() {
            bases.insert(s.id, resolved);
        }
    }
    // Methods declared directly on each type (callables whose parent is the type), plus
    // out-of-line methods resolved by the hierarchy (Rust impls, C++ `Type::m`).
    let members = |class: SymbolId, name: &str| -> Option<SymbolId> {
        hierarchy
            .method(class, name)
            .filter(|&m| !index.symbol(m).is_synthetic())
    };

    for s in &index.symbols {
        if !s.kind.is_type() || !bases.contains_key(&s.id) {
            continue;
        }
        let Some(own) = hierarchy.methods.get(&s.id) else {
            continue;
        };
        let mut own: Vec<(&String, &SymbolId)> = own.iter().collect();
        own.sort_by_key(|(_, m)| **m);
        for (name, &m) in own {
            let method = index.symbol(m);
            if method.is_synthetic() || method.kind == SymbolKind::Constructor {
                continue;
            }
            // Trait implementations come from impl relations below.
            if language_rules(method.language).trait_impls {
                continue;
            }
            let targets = if language_rules(s.language).overrides_follow_mro {
                python_base(index, &bases, s.id, name, &members)
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                nearest_definers(&bases, s.id, name, &members)
            };
            for base in targets {
                if base == m {
                    continue;
                }
                out.push(edge(index, m, base, kind_for(index, hierarchy, base), line_of));
            }
        }
    }

    // Out-of-line relations: `impl Trait for Type` (Rust), protocol / extension conformance
    // (`extension Type: Protocol`), and every other `FileFacts::impls` relation.
    let traits = relation_traits(index, &mut rules);
    let mut keys: Vec<(FileId, usize)> = traits.keys().copied().collect();
    keys.sort_unstable();
    for key in keys {
        let (fid, ri) = key;
        let tr = traits[&key];
        let file = index.file(fid);
        if !wanted(file.language) {
            continue;
        }
        let Some(rel) = file.facts.as_ref().and_then(|f| f.impls.get(ri)) else {
            continue;
        };
        let trait_impls = language_rules(file.language).trait_impls;
        if trait_impls && index.symbol(tr).kind != SymbolKind::Interface {
            continue;
        }
        let type_name = base_name(&rel.type_name);
        if type_name.is_empty() {
            continue;
        }
        let kind_of = |base: SymbolId| {
            if trait_impls {
                EdgeKind::Implements
            } else {
                kind_for(index, hierarchy, base)
            }
        };
        // Members declared inside the relation block (impl / extension body).
        for s in index.symbols_of(fid) {
            if !s.kind.is_callable()
                || s.is_synthetic()
                || s.kind == SymbolKind::Constructor
                || !rel.span.encloses(s.span.bytes)
                || s.container.as_deref().map(base_name) != Some(type_name)
            {
                continue;
            }
            if let Some(base) = members(tr, &s.name) {
                if base != s.id {
                    out.push(edge(index, s.id, base, kind_of(base), line_of));
                }
            }
        }
        // Other languages: a member of the conforming type declared anywhere (type body,
        // any extension of the type in any file) satisfies the requirement of its name.
        if trait_impls {
            continue;
        }
        let Some(ty) = rules.resolve_relation_type(fid, rel, tr) else {
            continue;
        };
        let Some(required) = hierarchy.methods.get(&tr) else {
            continue;
        };
        let mut required: Vec<(&String, &SymbolId)> = required.iter().collect();
        required.sort_by_key(|(_, m)| **m);
        for (name, &base) in required {
            let b = index.symbol(base);
            if b.is_synthetic() || b.kind == SymbolKind::Constructor {
                continue;
            }
            if let Some(m) = members(ty, name) {
                if m != base && index.symbol(m).kind != SymbolKind::Constructor {
                    out.push(edge(index, m, base, kind_of(base), line_of));
                }
            }
        }
    }
    // Naming-convention methods implement their generic (language rule; one generic per
    // method; `LanguageRules::generic_method_rule`).
    for (method, generic) in crate::hierarchy::convention_methods(index) {
        let language = index.symbol(method).language;
        if wanted(language) {
            let mut e = edge(index, method, generic, EdgeKind::Implements, line_of);
            e.provider = Provider::Rule(language_rules(language).generic_method_rule.into());
            out.push(e);
        }
    }
    out.extend(prototype_edges(index, line_of));
    // Members of other groups (a relation's members may be declared in another file of the
    // same group only; prototypes link within C/C++).
    out.retain(|e| wanted(index.symbol(e.from).language));
    // One edge per (from, to, kind); pairs a server already proved are not repeated.
    let proven: HashSet<(SymbolId, SymbolId, EdgeKind)> = index
        .edges
        .iter()
        .filter(|e| {
            matches!(e.kind, EdgeKind::Implements | EdgeKind::Overrides | EdgeKind::StubImplementation)
        })
        .map(|e| (e.from, e.to, e.kind))
        .collect();
    let mut seen: HashSet<(SymbolId, SymbolId, EdgeKind)> = HashSet::new();
    out.retain(|e| {
        let key = (e.from, e.to, e.kind);
        !proven.contains(&key) && seen.insert(key)
    });
    out
}

/// Declarations that are not definitions -> their definition (module docs): C / C++
/// prototypes and forward declarations, Haskell type signatures, TypeScript `declare` /
/// overload signatures. Proven `stub_implementation` edges.
pub(crate) fn prototype_edges(index: &Index, line_of: Option<LineOf<'_>>) -> Vec<Edge> {
    let mut out = c_prototype_edges(index, line_of);
    out.extend(signature_edges(index, line_of));
    out
}

/// C / C++ prototypes and forward declarations (bodiless callable / type declarations,
/// `is_stub`) -> their definition: proven `stub_implementation` edges (provider
/// `rule:c-prototype`, resolution `abi_naming_rule`) when exactly one C/C++ definition of the
/// same kind with the same name and a matching owner path exists in the index (C symbol
/// names are global; C++ has one definition per entity;
/// [`trace_core::assemble::c_prototype_definitions`], the rule the link also uses for
/// canonical call targets). Several definitions (static functions of several files,
/// overloads) produce nothing.
fn c_prototype_edges(index: &Index, line_of: Option<LineOf<'_>>) -> Vec<Edge> {
    trace_core::assemble::c_prototype_definitions(&index.symbols)
        .into_iter()
        .map(|(proto, def)| {
            let mut e = edge(index, proto, def, EdgeKind::StubImplementation, line_of);
            e.provider = Provider::Rule("c-prototype".into());
            e.resolution = Resolution::AbiNamingRule;
            e
        })
        .collect()
}

/// Haskell type signatures -> every equation of the binding (same file, scope and name);
/// TypeScript `declare` / overload signatures -> the unique implementation with the same
/// qualified name in the same file. Provider `rule:<language>-declaration`, resolution
/// `inheritance_rule`.
fn signature_edges(index: &Index, line_of: Option<LineOf<'_>>) -> Vec<Edge> {
    let applies = |l: Language| language_rules(l).signatures != Signatures::None;
    type Key<'k> = (FileId, Option<SymbolId>, &'k str);
    let mut definitions: HashMap<Key<'_>, Vec<SymbolId>> = HashMap::new();
    for s in &index.symbols {
        if applies(s.language) && s.kind.is_callable() && !s.is_stub && !s.is_synthetic() {
            definitions
                .entry((s.file, s.parent, s.qualified_name.as_str()))
                .or_default()
                .push(s.id);
        }
    }
    let mut out = Vec::new();
    for s in &index.symbols {
        if !(applies(s.language) && s.kind.is_callable() && s.is_stub) || s.is_synthetic() {
            continue;
        }
        let Some(defs) = definitions.get(&(s.file, s.parent, s.qualified_name.as_str())) else {
            continue;
        };
        let targets: &[SymbolId] = match language_rules(s.language).signatures {
            // One binding, one equation per clause.
            Signatures::EveryEquation => defs.as_slice(),
            _ if defs.len() == 1 => defs.as_slice(),
            _ => &[],
        };
        for &t in targets {
            let mut e = edge(index, s.id, t, EdgeKind::StubImplementation, line_of);
            e.provider = Provider::Rule(format!("{}-declaration", s.language.as_str()));
            e.resolution = Resolution::InheritanceRule;
            out.push(e);
        }
    }
    out
}

fn kind_for(index: &Index, hierarchy: &Hierarchy, base: SymbolId) -> EdgeKind {
    let b = index.symbol(base);
    let interface = hierarchy
        .class_of(index, base)
        .is_some_and(|c| index.symbol(c).kind == SymbolKind::Interface);
    if b.is_stub || interface {
        EdgeKind::Implements
    } else {
        EdgeKind::Overrides
    }
}

fn edge(index: &Index, from: SymbolId, to: SymbolId, kind: EdgeKind, line_of: Option<LineOf<'_>>) -> Edge {
    let s = index.symbol(from);
    let line = line_of
        .and_then(|f| f(s.file, s.name_span.start))
        .unwrap_or_else(|| approximate_line(index, from));
    Edge {
        from,
        to,
        kind,
        tier: Tier::Proven,
        provider: Provider::Rule(rule_name(s.language)),
        resolution: Resolution::InheritanceRule,
        at: Location {
            file: s.file,
            bytes: s.name_span,
            line,
        },
        site: None,
        bridge: None,
    }
}

/// Declaration line of the name: start line plus the lines of preceding decorators.
fn approximate_line(index: &Index, id: SymbolId) -> u32 {
    let s = index.symbol(id);
    let decorator_lines: u32 = s.decorators.iter().map(|d| d.matches('\n').count() as u32 + 1).sum();
    s.span.start_line + decorator_lines
}

/// Python: first class after `class` in its C3 linearization defining `name`.
fn python_base(
    index: &Index,
    bases: &HashMap<SymbolId, Vec<SymbolId>>,
    class: SymbolId,
    name: &str,
    members: &dyn Fn(SymbolId, &str) -> Option<SymbolId>,
) -> Option<SymbolId> {
    let order = c3(bases, class, &mut HashSet::new()).unwrap_or_else(|| depth_first(bases, class));
    order
        .into_iter()
        .skip(1)
        .filter(|&k| index.symbol(k).kind.is_type())
        .find_map(|k| members(k, name))
}

/// C3 linearization (None when inconsistent or cyclic).
fn c3(
    bases: &HashMap<SymbolId, Vec<SymbolId>>,
    class: SymbolId,
    visiting: &mut HashSet<SymbolId>,
) -> Option<Vec<SymbolId>> {
    if !visiting.insert(class) {
        return None;
    }
    let direct = bases.get(&class).cloned().unwrap_or_default();
    let mut seqs: Vec<VecDeque<SymbolId>> = Vec::new();
    for &b in &direct {
        seqs.push(c3(bases, b, visiting)?.into());
    }
    seqs.push(direct.iter().copied().collect());
    visiting.remove(&class);
    let mut out = vec![class];
    loop {
        seqs.retain(|s| !s.is_empty());
        if seqs.is_empty() {
            return Some(out);
        }
        let head = seqs
            .iter()
            .map(|s| s[0])
            .find(|&h| seqs.iter().all(|s| !s.iter().skip(1).any(|&x| x == h)))?;
        out.push(head);
        for s in &mut seqs {
            if s.front() == Some(&head) {
                s.pop_front();
            }
        }
    }
}

fn depth_first(bases: &HashMap<SymbolId, Vec<SymbolId>>, class: SymbolId) -> Vec<SymbolId> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![class];
    while let Some(c) = stack.pop() {
        if !seen.insert(c) {
            continue;
        }
        out.push(c);
        if let Some(bs) = bases.get(&c) {
            stack.extend(bs.iter().rev());
        }
    }
    out
}

/// Breadth-first over resolved bases; on each path the nearest class defining `name`.
fn nearest_definers(
    bases: &HashMap<SymbolId, Vec<SymbolId>>,
    class: SymbolId,
    name: &str,
    members: &dyn Fn(SymbolId, &str) -> Option<SymbolId>,
) -> Vec<SymbolId> {
    let mut out: BTreeSet<SymbolId> = BTreeSet::new();
    let mut seen: HashSet<SymbolId> = HashSet::from([class]);
    let mut todo: VecDeque<SymbolId> = bases.get(&class).cloned().unwrap_or_default().into();
    while let Some(k) = todo.pop_front() {
        if !seen.insert(k) {
            continue;
        }
        match members(k, name) {
            Some(m) => {
                out.insert(m);
            }
            None => todo.extend(bases.get(&k).map(Vec::as_slice).unwrap_or(&[])),
        }
    }
    out.into_iter().collect()
}

/// Base resolution (module docs, rules 1-4).
struct Rules<'i> {
    index: &'i Index,
    /// Type name -> type symbols (non-synthetic).
    types: HashMap<&'i str, Vec<SymbolId>>,
    /// (file, span start) -> semantic value reference targets.
    refs: HashMap<FileId, Vec<(ByteSpan, SymbolId)>>,
    modules: Option<ModuleMap>,
}

impl<'i> Rules<'i> {
    fn new(index: &'i Index, _hierarchy: &Hierarchy) -> Rules<'i> {
        let mut types: HashMap<&str, Vec<SymbolId>> = HashMap::new();
        for s in &index.symbols {
            if s.kind.is_type() && !s.is_synthetic() {
                types.entry(s.name.as_str()).or_default().push(s.id);
            }
        }
        let mut refs: HashMap<FileId, Vec<(ByteSpan, SymbolId)>> = HashMap::new();
        for r in &index.value_refs {
            if index.symbol(r.target).kind.is_type() {
                refs.entry(r.at.file).or_default().push((r.at.bytes, r.target));
            }
        }
        for e in &index.edges {
            if matches!(e.kind, EdgeKind::References | EdgeKind::Imports) && index.symbol(e.to).kind.is_type()
            {
                refs.entry(e.at.file).or_default().push((e.at.bytes, e.to));
            }
        }
        for list in refs.values_mut() {
            list.sort_unstable_by_key(|(s, t)| (s.start, s.end, *t));
            list.dedup();
        }
        Rules {
            index,
            types,
            refs,
            modules: None,
        }
    }

    /// Types named `name` that a compiler fact inside `header` of `file` refers to (a value
    /// reference or reference edge at an identifier in the header): `None` without such a
    /// fact, `Some(Some(t))` for exactly one, `Some(None)` for several (ambiguous).
    fn header_fact(
        &self,
        file: FileId,
        header: ByteSpan,
        name: &str,
        exclude: Option<SymbolId>,
    ) -> Option<Option<SymbolId>> {
        let index = self.index;
        let list = self.refs.get(&file)?;
        let from = list.partition_point(|(span, _)| span.start < header.start);
        let found: BTreeSet<SymbolId> = list[from..]
            .iter()
            .take_while(|(span, _)| span.start < header.end)
            .filter(|(span, t)| {
                header.encloses(*span) && Some(*t) != exclude && index.symbol(*t).name == name
            })
            .map(|(_, t)| *t)
            .collect();
        match found.len() {
            0 => None,
            1 => Some(found.into_iter().next()),
            _ => Some(None),
        }
    }

    /// The unique class a base spelling of `class` names.
    fn resolve_base(&mut self, class: SymbolId, spelling: &str) -> Option<SymbolId> {
        let name = base_name(spelling);
        if name.is_empty() {
            return None;
        }
        let index = self.index;
        let s = index.symbol(class);
        // 1. Compiler fact inside the class header.
        let header = ByteSpan::new(s.span.bytes.start, s.body_start.max(s.span.bytes.start));
        if let Some(found) = self.header_fact(s.file, header, name, Some(class)) {
            return found;
        }
        let qualifier = qualifier_of(spelling, name);
        self.resolve_name(s.file, name, &qualifier).filter(|&b| b != class)
    }

    /// Header of an out-of-line relation: from its start to the first declaration inside it
    /// (the whole relation when it declares nothing).
    fn relation_header(&self, file: FileId, rel: &ImplRelation) -> ByteSpan {
        let end = self
            .index
            .symbols_of(file)
            .iter()
            .filter(|s| rel.span.encloses(s.span.bytes) && s.span.bytes.start > rel.span.start)
            .map(|s| s.span.bytes.start)
            .min()
            .unwrap_or(rel.span.end);
        ByteSpan::new(rel.span.start, end.max(rel.span.start))
    }

    /// The trait / protocol / interface an out-of-line relation names: a compiler fact in
    /// the relation header first, then the unique-name rules 2-4.
    fn resolve_relation_trait(&mut self, file: FileId, rel: &ImplRelation) -> Option<SymbolId> {
        let name = base_name(&rel.trait_name);
        if name.is_empty() {
            return None;
        }
        let header = self.relation_header(file, rel);
        if let Some(found) = self.header_fact(file, header, name, None) {
            return found;
        }
        let qualifier = qualifier_of(&rel.trait_name, name);
        self.resolve_name(file, name, &qualifier)
    }

    /// The conforming type of an out-of-line relation (same order as the trait).
    fn resolve_relation_type(
        &mut self,
        file: FileId,
        rel: &ImplRelation,
        trait_: SymbolId,
    ) -> Option<SymbolId> {
        let name = base_name(&rel.type_name);
        if name.is_empty() {
            return None;
        }
        let header = self.relation_header(file, rel);
        if let Some(found) = self.header_fact(file, header, name, Some(trait_)) {
            return found;
        }
        let qualifier = qualifier_of(&rel.type_name, name);
        self.resolve_name(file, name, &qualifier).filter(|&t| t != trait_)
    }

    /// Rules 2-4 for a type name used in `file` (`qualifier`: leading segments of a dotted
    /// spelling; its first segment may be bound by an import).
    fn resolve_name(&mut self, file: FileId, name: &str, qualifier: &[&str]) -> Option<SymbolId> {
        let index = self.index;
        let language = index.file(file).language;
        let all: Vec<SymbolId> = self
            .types
            .get(name)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&t| same_family(index.symbol(t).language, language))
                    .collect()
            })
            .unwrap_or_default();
        if all.is_empty() {
            return None;
        }
        // 2. Same file (only when the spelling is not qualified).
        if qualifier.is_empty() {
            let local: Vec<SymbolId> = all
                .iter()
                .copied()
                .filter(|&t| index.symbol(t).file == file)
                .collect();
            match local.len() {
                1 => return Some(local[0]),
                0 => {}
                _ => return None,
            }
        }
        // 3. An import binding the root name (or the name itself): it decides alone; an
        //    import of something outside the repository names no class of the index.
        let local = qualifier.first().copied().unwrap_or(name);
        match self.import_files(file, local, &qualifier[qualifier.len().min(1)..], name) {
            Some(Some(files)) => {
                let hits: Vec<SymbolId> = all
                    .iter()
                    .copied()
                    .filter(|&t| files.contains(&index.symbol(t).file))
                    .collect();
                return (hits.len() == 1).then(|| hits[0]);
            }
            Some(None) => return None,
            None => {}
        }
        // 4. Globally unique name.
        (all.len() == 1).then(|| all[0])
    }

    /// Files the import binding `local` refers to: `None` when no import binds it,
    /// `Some(None)` when its target is outside the repository. `rest`: qualifier segments
    /// after the import-bound root (`pkg.mod.Base` imported as `pkg`: `[mod]`).
    fn import_files(
        &mut self,
        file: FileId,
        local: &str,
        rest: &[&str],
        name: &str,
    ) -> Option<Option<Vec<FileId>>> {
        let index = self.index;
        let facts = index.file(file).facts.as_ref()?;
        let import = facts
            .imports
            .iter()
            .rev()
            .find(|i| i.local == local && i.kind != ImportKind::Wildcard)?;
        let path = index.file_path(file);
        let language = index.file(file).language;
        let bound_is_name = local == name;
        let target = if rest.is_empty() {
            import.target.clone()
        } else {
            format!("{}.{}", import.target, rest.join("."))
        };
        let member = bound_is_name && import.kind == ImportKind::Member;
        let modules = self.modules.get_or_insert_with(|| ModuleMap::new(index));
        let Some((mut files, _)) = modules
            .resolve(path, language, &target, member)
            .or_else(|| modules.resolve(path, language, &target, true))
        else {
            return Some(None);
        };
        // One re-export hop (`export { Base } from`, `__all__` imports, `pub use`).
        let mut extra = Vec::new();
        for &f in &files {
            let Some(ff) = index.file(f).facts.as_ref() else {
                continue;
            };
            for e in &ff.exports {
                if e.exported == name || e.exported == "*" {
                    let exported_member = e.exported != "*";
                    if let Some((more, _)) = modules.resolve(
                        index.file_path(f),
                        index.file(f).language,
                        &e.target,
                        exported_member,
                    ) {
                        extra.extend(more);
                    }
                }
            }
        }
        files.extend(extra);
        files.sort_unstable();
        files.dedup();
        Some(Some(files))
    }
}

#[cfg(test)]
#[path = "../tests/unit/family.rs"]
mod tests;
