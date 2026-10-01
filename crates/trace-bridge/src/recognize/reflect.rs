//! Installed annotation declarations for reflection chains (DESIGN-bridges §2 rule 5).
//!
//! An annotation a repository declaration carries names its type through the file's imports
//! (`import a.b.GetRoute;`, `import a.b.*;`) or a qualified spelling. The type's declaration
//! in the installed packages ([`trace_library::reflect::types`]) carries its meta-annotations;
//! those name further types through the declaring file's own imports and package, followed
//! (bounded) until the chain reaches the reflection roots. The recognizer's chain walk
//! ([`trace_library::channels::annotation_chain`]) then reads the collected declarations like
//! the repository's own annotation types.
//!
//! C# attributes are compiled classes: the attribute's lineage in the installed assemblies'
//! metadata (the class, then its base classes, each with its declared interfaces) reaches a
//! root when one of those interfaces is a `reflection_roots` row. When the lineage reaches a
//! row that selects a verb, the HTTP method tokens among the string literals the lineage's
//! own code loads (a constructor handing its method list to the base) are the chain's verb.
//!
//! JavaScript / TypeScript decorators are code: the metadata a decorator factory call stores
//! on the decorated class or member is evaluated from the installed implementation
//! ([`trace_library::reflect::decorator_meta`]) up to the metadata-store root row. A member decorator
//! registers a route when it stores exactly one HTTP method token (a constant or a compiled
//! enumeration member named like one) and exactly one value built from its own arguments (the
//! path; its constant default when the argument is absent). A class decorator storing a value
//! under the same metadata key gives the class's prefix (as the scanner joins class and member
//! metadata of one key); an ambiguous prefix gives no route.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use trace_core::facts::{BoundaryFact, BoundaryRole, FileFacts, Import, ImportKind};
use trace_core::languages::{in_family, Family};
use trace_core::model::BridgeKind;
use trace_core::Language;
use trace_env::LibraryRoot;
use trace_library::channels::{http_method_token, ChannelRow, ReflectionChain};
use trace_library::model::ArgSel;
use trace_library::reflect::decorator_meta::{decorator_writes, MetaValue, MetaWrite};
use trace_syntax::boundary::{Annotation, Tpl, TplPart};

use super::*;
use crate::http::normalize;

/// Annotation types followed per file at most.
const MAX_TYPES: usize = 24;

/// Qualified names `written` may stand for in a file with `imports` declared in `package`
/// (most specific first): the qualified spelling itself, the single-type import binding its
/// first segment, then every on-demand import and the file's own package.
fn candidates(written: &str, imports: &[Import], package: Option<&str>) -> Vec<String> {
    let written = written.trim_start_matches('@');
    let (first, rest) = match written.split_once('.') {
        Some((f, r)) => (f, Some(r)),
        None => (written, None),
    };
    let mut out = Vec::new();
    if let Some(i) = imports
        .iter()
        .find(|i| i.kind != ImportKind::Wildcard && i.local == first && !i.target.is_empty())
    {
        out.push(match rest {
            Some(r) => format!("{}.{r}", i.target),
            None => i.target.clone(),
        });
        return out;
    }
    if rest.is_some() {
        out.push(written.to_string());
    }
    for i in imports.iter().filter(|i| i.kind == ImportKind::Wildcard) {
        let base = i
            .target
            .trim_end_matches(".*")
            .trim_end_matches('*')
            .trim_end_matches('.');
        if !base.is_empty() {
            out.push(format!("{base}.{written}"));
        }
    }
    if let Some(p) = package.filter(|p| !p.is_empty()) {
        out.push(format!("{p}.{written}"));
    }
    out
}

/// Package a Java source declares (`package a.b;`), from its syntax facts' module name.
fn declared_package(source: &[u8]) -> Option<String> {
    let tree = trace_syntax::parse_tree(Language::Java, source).ok()?;
    let root = tree.root_node();
    let mut cursor = root.walk();
    let decl = root
        .named_children(&mut cursor)
        .find(|n| n.kind() == "package_declaration")?;
    let mut c2 = decl.walk();
    let name = decl
        .named_children(&mut c2)
        .find(|n| matches!(n.kind(), "scoped_identifier" | "identifier"))?;
    name.utf8_text(source).ok().map(|t| t.trim().to_string())
}

fn imports_of(language: Language, source: &[u8]) -> Vec<Import> {
    trace_syntax::extract(trace_syntax::SourceInput {
        path: "<library>",
        language,
        source,
    })
    .map(|f| f.imports)
    .unwrap_or_default()
}

/// Declarations (sources) of the installed annotation types behind `written` annotation
/// names of one repository file, with the types their meta-annotations name, stopping at
/// names in `stop` (the reflection roots' simple names). Java only (the language whose
/// installed annotation types are source files).
pub(crate) fn installed_annotation_sources(
    roots: &[LibraryRoot],
    language: Language,
    facts: &FileFacts,
    written: &BTreeSet<String>,
    stop: &BTreeSet<String>,
) -> Vec<Vec<u8>> {
    if language != Language::Java || roots.is_empty() || written.is_empty() {
        return Vec::new();
    }
    let simple = |s: &str| s.rsplit('.').next().unwrap_or(s).to_string();
    // Types of the repository file's own package are repository sources (read already).
    let mut queue: VecDeque<String> = VecDeque::new();
    for w in written {
        if stop.contains(&simple(w)) {
            continue;
        }
        queue.extend(candidates(w, &facts.imports, None));
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut found_names: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<Vec<u8>> = Vec::new();
    while let Some(q) = queue.pop_front() {
        if seen.len() >= MAX_TYPES * 4 || !seen.insert(q.clone()) {
            continue;
        }
        // One declaration per simple name (the first candidate that resolves).
        if found_names.contains(&simple(&q)) || found_names.len() >= MAX_TYPES {
            continue;
        }
        let found = trace_library::reflect::types::type_sources(roots, language, &q);
        let Some((_, bytes)) = found.into_iter().next() else { continue };
        found_names.insert(simple(&q));
        let imports = imports_of(language, &bytes);
        let own_package = declared_package(&bytes).or_else(|| q.rsplit_once('.').map(|(p, _)| p.to_string()));
        for t in trace_syntax::lower::annotation_types(language, &bytes) {
            for meta in &t.annotations {
                if stop.contains(&simple(&meta.name)) {
                    continue;
                }
                queue.extend(candidates(&meta.name, &imports, own_package.as_deref()));
            }
        }
        out.push(bytes);
    }
    out
}

impl Recognizer<'_, '_> {
    /// Installed declarations of the annotation types the file's declarations carry
    /// (reflection chains up to `roots`).
    pub(super) fn installed_annotations(&self, site: &Site<'_>, roots: &[ChannelRow]) -> Vec<Vec<u8>> {
        let language = site.rec.language;
        if language != Language::Java || roots.is_empty() {
            return Vec::new();
        }
        let stop: BTreeSet<String> = roots
            .iter()
            .filter_map(|r| r.symbol.as_deref())
            .map(simple_name)
            .collect();
        installed_annotation_sources(
            self.input.installed.roots(),
            language,
            site.facts,
            &written_annotations(site),
            &stop,
        )
    }

    /// Chains of the compiled attribute types the file's declarations carry (C#).
    pub(super) fn compiled_chains(
        &self,
        site: &Site<'_>,
        roots: &[ChannelRow],
    ) -> BTreeMap<String, ReflectionChain> {
        if site.rec.language != Language::CSharp || roots.is_empty() {
            return BTreeMap::new();
        }
        compiled_attribute_chains(self.input.installed.roots(), site.facts, &written_annotations(site), roots)
    }
}

impl Recognizer<'_, '_> {
    /// Routes registered by decorator metadata (JavaScript / TypeScript classes).
    pub(super) fn decorator_routes(
        &self,
        site: &Site<'_>,
        roots: &[ChannelRow],
        out: &mut Vec<BoundaryFact>,
    ) {
        if !in_family(site.rec.language, Family::JavaScript) {
            return;
        }
        let Some(sem) = site.semantics else { return };
        let stores: Vec<(&str, u32)> = roots
            .iter()
            .filter_map(|r| match (&r.symbol, &r.key) {
                (Some(s), Some(ArgSel::Pos(k))) => Some((s.as_str(), *k)),
                _ => None,
            })
            .collect();
        if stores.is_empty() {
            return;
        }
        let facts = site.facts;
        let mut cache: BTreeMap<(u32, u32, u32, u32), Vec<MetaWrite>> = BTreeMap::new();
        // The positional arguments of a decorator call (`Some(text)`: a written string) and
        // the metadata writes of the decorator it returns.
        let mut applied = |a: &Annotation| -> Option<(Vec<Option<String>>, Vec<MetaWrite>)> {
            let call = facts.calls.iter().find(|c| {
                c.callee_span.start >= a.span.start && c.callee_span.end <= a.span.end && c.callee == a.name
            })?;
            let lc = sem.library_calls.iter().find(|l| l.at == call.callee_span)?;
            let lib = sem.library_files.get(lc.file as usize).filter(|f| f.readable)?;
            let mut args = Vec::new();
            for i in 0..16u32 {
                let Some(v) = self.arg(site, call.callee_span, &ArgSel::Pos(i)) else { break };
                args.push(v.template.as_ref().and_then(Tpl::plain));
            }
            let key = (lc.file, lc.decl_line, lc.decl_column, args.len() as u32);
            let writes = cache
                .entry(key)
                .or_insert_with(|| {
                    stores
                        .iter()
                        .flat_map(|(root, k)| {
                            decorator_writes(
                                Path::new(&lib.path),
                                lc.decl_line,
                                lc.decl_column,
                                args.len() as u32,
                                root,
                                *k,
                            )
                        })
                        .collect()
                })
                .clone();
            Some((args, writes))
        };
        for (ci, class) in facts.declarations.iter().enumerate() {
            if !class.kind.is_type() {
                continue;
            }
            // (Decorators of an exported class sit on the export statement.) A decorator call
            // without a known library declaration may store the prefix: unknown prefix.
            let mut class_writes: Vec<(Vec<Option<String>>, Vec<MetaWrite>)> = Vec::new();
            let mut prefix_unknown = false;
            for a in site.parsed.annotations(class.span.bytes) {
                match applied(&a) {
                    Some(found) => class_writes.push(found),
                    None => {
                        prefix_unknown |= facts
                            .calls
                            .iter()
                            .any(|c| c.callee_span.start >= a.span.start && c.callee_span.end <= a.span.end)
                    }
                }
            }
            for (mi, member) in facts.declarations.iter().enumerate() {
                if member.parent != Some(ci as u32)
                    || member.decorators.is_empty()
                    || !member.kind.is_callable()
                {
                    continue;
                }
                for a in site.parsed.annotations(member.span.bytes) {
                    let Some((args, writes)) = applied(&a) else { continue };
                    let Some((path_key, verb, path)) = member_route(&args, &writes) else { continue };
                    // The class prefix: class metadata under the path's key (one value, else no
                    // route).
                    let mut prefixes: BTreeSet<Option<String>> = BTreeSet::new();
                    for (cargs, cw) in &class_writes {
                        for w in cw.iter().filter(|w| !w.on_member && w.key == path_key) {
                            prefixes.insert(resolve(&w.value, cargs).and_then(single));
                        }
                    }
                    let prefix = match prefixes.len() {
                        0 => String::new(),
                        1 => match prefixes.into_iter().next().flatten() {
                            Some(p) => p,
                            None => continue,
                        },
                        _ => continue,
                    };
                    let mut parts = vec![TplPart::Lit(prefix), TplPart::Lit("/".into()), TplPart::Lit(path)];
                    if prefix_unknown {
                        parts.insert(0, TplPart::Hole(String::new()));
                    }
                    let tpl = Tpl {
                        parts,
                        dynamic: prefix_unknown,
                    };
                    let Some(norm) = normalize(&tpl, false, &self.placeholders) else { continue };
                    let via = stores.first().map(|(r, _)| r.to_string()).unwrap_or_default();
                    let mut detail = common_detail(&via, true);
                    detail.push(("framework".into(), format!("reflection root ({via})")));
                    detail.push(("method".into(), verb.clone()));
                    detail.push(("path".into(), norm.path.clone()));
                    if norm.dynamic || norm.dynamic_prefix {
                        detail.push(("dynamic".into(), "true".into()));
                    }
                    out.push(fact(
                        site,
                        BridgeKind::Http,
                        BoundaryRole::Provides,
                        format!("{verb} {}", norm.path),
                        None,
                        Some(mi as u32),
                        a.span,
                        detail,
                    ));
                }
            }
        }
    }
}

/// The one string of a resolved value.
fn single(values: Vec<String>) -> Option<String> {
    let set: BTreeSet<String> = values.into_iter().collect();
    (set.len() == 1).then(|| set.into_iter().next().unwrap_or_default())
}

/// Whether a stored value is built from the factory call's arguments.
fn from_arguments(v: &MetaValue) -> bool {
    match v {
        MetaValue::Arg(_) | MetaValue::ArgField(..) => true,
        MetaValue::Choice(c) => c.iter().any(from_arguments),
        _ => false,
    }
}

/// Strings a stored value takes at a call with positional `args` (`Some(text)` for a written
/// string, `None` for another value): the passed argument's alternatives when the call passes
/// it, else the constant defaults; `None` when a passed value is not a string.
fn resolve(v: &MetaValue, args: &[Option<String>]) -> Option<Vec<String>> {
    fn walk(
        v: &MetaValue,
        args: &[Option<String>],
        passed: &mut Vec<String>,
        defaults: &mut Vec<String>,
    ) -> bool {
        match v {
            MetaValue::Arg(i) => match args.get(*i as usize) {
                Some(Some(s)) => {
                    passed.push(s.clone());
                    true
                }
                Some(None) => false,
                None => true,
            },
            // A property of a written string is absent; of another value, unknown.
            MetaValue::ArgField(i, _) => !matches!(args.get(*i as usize), Some(None)),
            MetaValue::Str(s) | MetaValue::Member(s) => {
                defaults.push(s.clone());
                true
            }
            MetaValue::Choice(c) => c.iter().all(|x| walk(x, args, passed, defaults)),
            MetaValue::Other => true,
        }
    }
    let (mut passed, mut defaults) = (Vec::new(), Vec::new());
    if !walk(v, args, &mut passed, &mut defaults) {
        return None;
    }
    Some(if passed.is_empty() { defaults } else { passed })
}

/// `(path key, verb, path)` of a member decorator's writes: exactly one HTTP method token
/// stored, exactly one key storing a value built from the arguments.
fn member_route(args: &[Option<String>], writes: &[MetaWrite]) -> Option<(String, String, String)> {
    let member: Vec<&MetaWrite> = writes.iter().filter(|w| w.on_member).collect();
    let verbs: Vec<String> = member
        .iter()
        .filter(|w| !from_arguments(&w.value))
        .filter_map(|w| resolve(&w.value, args))
        .filter(|vals| !vals.is_empty() && vals.iter().all(|t| http_method_token(t).is_some()))
        .flatten()
        .filter_map(|t| http_method_token(&t).map(str::to_string))
        .collect();
    let verb = single(verbs)?;
    let keys: BTreeSet<&str> = member
        .iter()
        .filter(|w| from_arguments(&w.value))
        .map(|w| w.key.as_str())
        .collect();
    if keys.len() != 1 {
        return None;
    }
    let key = keys.into_iter().next()?.to_string();
    let values: Vec<String> = member
        .iter()
        .filter(|w| w.key == key)
        .map(|w| resolve(&w.value, args))
        .collect::<Option<Vec<Vec<String>>>>()?
        .into_iter()
        .flatten()
        .collect();
    let path = single(values)?;
    Some((key, verb, path))
}

/// Annotation / attribute names written on the file's declarations.
fn written_annotations(site: &Site<'_>) -> BTreeSet<String> {
    site.facts
        .declarations
        .iter()
        .filter(|d| !d.decorators.is_empty())
        .flat_map(|d| site.parsed.annotations(d.span.bytes))
        .map(|a| a.name)
        .collect()
}

/// Reflection chains of compiled attribute types (C#): `written` attribute name -> chain,
/// for the names whose installed lineage implements a root row's interface.
pub(crate) fn compiled_attribute_chains(
    roots: &[LibraryRoot],
    facts: &FileFacts,
    written: &BTreeSet<String>,
    rows: &[ChannelRow],
) -> BTreeMap<String, ReflectionChain> {
    let mut out = BTreeMap::new();
    for w in written {
        let mut names = vec![w.clone()];
        if !w.ends_with("Attribute") {
            // The language's attribute naming rule: `[X]` names the class `XAttribute`.
            names.insert(0, format!("{w}Attribute"));
        }
        let lineage = names
            .iter()
            .flat_map(|n| candidates(n, &facts.imports, None))
            .map(|q| trace_library::reflect::types::clr_lineage(roots, &q))
            .find(|l| !l.is_empty());
        let Some(lineage) = lineage else { continue };
        let interfaces: BTreeSet<String> = lineage
            .iter()
            .flat_map(|t| t.def.interfaces.iter().map(|i| i.full()))
            .collect();
        let reached: Vec<&ChannelRow> = rows
            .iter()
            .filter(|r| r.symbol.as_deref().is_some_and(|s| interfaces.contains(s)))
            .collect();
        // The path root first (a row with a key), else a verb root.
        let Some(root) = reached.iter().find(|r| r.key.is_some()).or_else(|| reached.first()) else {
            continue;
        };
        let verb = if reached.iter().any(|r| r.verb.is_some()) {
            let tokens: BTreeSet<&str> = lineage
                .iter()
                .flat_map(|t| t.literals.iter())
                .filter_map(|l| http_method_token(l))
                .collect();
            // One verb, or none claimed.
            (tokens.len() == 1).then(|| tokens.into_iter().next().unwrap_or_default().to_string())
        } else {
            None
        };
        let Some(symbol) = root.symbol.as_deref() else { continue };
        out.insert(
            w.clone(),
            ReflectionChain {
                annotation: lineage[0].def.name.clone(),
                root: simple_name(symbol),
                verb,
                path_elements: Vec::new(),
            },
        );
    }
    out
}

// ---- reflection-driven registration

impl Recognizer<'_, '_> {
    /// Whether the file's language has reflection-root or attribute rows and the file has
    /// annotated declarations.
    pub(super) fn reflective(&self, rec: &FileRecord, facts: &FileFacts) -> bool {
        let has_rows = !self
            .input
            .tables
            .irreducible(rec.language, Section::ReflectionRoots)
            .is_empty()
            || self
                .input
                .tables
                .irreducible(rec.language, Section::FfiConventions)
                .iter()
                .any(|r| r.symbol.is_some() && r.key.is_some());
        has_rows && facts.declarations.iter().any(|d| !d.decorators.is_empty())
    }

    pub(super) fn reflection(&self, site: &Site<'_>, out: &mut Vec<BoundaryFact>) {
        let language = site.rec.language;
        let roots = self.active_rows(language, Section::ReflectionRoots);
        let ffi_rows: Vec<ChannelRow> = self
            .active_rows(language, Section::FfiConventions)
            .into_iter()
            .filter(|r| r.symbol.is_some() && r.key.is_some())
            .collect();
        let mut sources: Vec<&[u8]> = self
            .annotation_sources
            .get(&language)
            .map(|v| v.iter().map(Vec::as_slice).collect())
            .unwrap_or_default();
        let library: Vec<Vec<u8>> = site
            .semantics
            .map(|s| {
                s.library_files
                    .iter()
                    .filter(|f| f.readable && f.language == language)
                    .filter_map(|f| std::fs::read(Path::new(&f.path)).ok())
                    .collect()
            })
            .unwrap_or_default();
        sources.extend(library.iter().map(Vec::as_slice));
        let facts = site.facts;
        let installed = self.installed_annotations(site, &roots);
        sources.extend(installed.iter().map(Vec::as_slice));
        let compiled = self.compiled_chains(site, &roots);
        self.decorator_routes(site, &roots, out);
        for (i, d) in facts.declarations.iter().enumerate() {
            if d.decorators.is_empty() || !d.kind.is_callable() || Some(i as u32) == facts.module_decl {
                continue;
            }
            let annotations = site.parsed.annotations(d.span.bytes);
            if annotations.is_empty() {
                continue;
            }
            // Platform-invoke style attributes: the declaration imports a native symbol.
            for a in &annotations {
                for row in &ffi_rows {
                    let Some(symbol) = row.symbol.as_deref() else { continue };
                    if !same_annotation(language, &a.name, symbol) {
                        continue;
                    }
                    let name = a
                        .elements
                        .iter()
                        .find(|(k, _)| k.as_deref().is_some_and(|k| k.ends_with("EntryPoint")))
                        .and_then(|(_, t)| t.plain())
                        .unwrap_or_else(|| d.name.clone());
                    let mut detail = common_detail(symbol, false);
                    detail.push(("loader".into(), symbol.to_string()));
                    out.push(fact(
                        site,
                        BridgeKind::Ffi,
                        BoundaryRole::Uses,
                        name,
                        None,
                        Some(i as u32),
                        a.span,
                        detail,
                    ));
                }
            }
            if roots.is_empty() {
                continue;
            }
            let method = self.route_parts(language, &annotations, &roots, &sources, &compiled);
            if method.paths.is_empty() && method.verbs.is_empty() {
                continue;
            }
            let class = d
                .parent
                .and_then(|p| facts.declarations.get(p as usize))
                .filter(|p| p.kind.is_type())
                .map(|p| {
                    self.route_parts(
                        language,
                        &site.parsed.annotations(p.span.bytes),
                        &roots,
                        &sources,
                        &compiled,
                    )
                })
                .unwrap_or_default();
            let class_paths = if class.paths.is_empty() {
                vec![Tpl::literal("")]
            } else {
                class.paths.clone()
            };
            let method_paths = if method.paths.is_empty() {
                vec![Tpl::literal("")]
            } else {
                method.paths.clone()
            };
            let verbs = if method.verbs.is_empty() {
                vec!["*".to_string()]
            } else {
                method.verbs.clone()
            };
            let derived = method.derived || class.derived;
            let span = annotations.first().map(|a| a.span).unwrap_or(d.name_span);
            for cp in &class_paths {
                for mp in &method_paths {
                    let mut full = cp.clone();
                    full.parts.push(TplPart::Lit("/".into()));
                    full.parts.extend(mp.parts.iter().cloned());
                    full.dynamic |= mp.dynamic;
                    let Some(norm) = normalize(&full, false, &self.placeholders) else { continue };
                    for m in &verbs {
                        let mut detail = common_detail(&method.via.clone().unwrap_or_default(), derived);
                        detail.push((
                            "framework".into(),
                            format!("reflection root ({})", method.via.clone().unwrap_or_default()),
                        ));
                        detail.push(("method".into(), m.clone()));
                        detail.push(("path".into(), norm.path.clone()));
                        if norm.dynamic || norm.dynamic_prefix {
                            detail.push(("dynamic".into(), "true".into()));
                        }
                        out.push(fact(
                            site,
                            BridgeKind::Http,
                            BoundaryRole::Provides,
                            format!("{m} {}", norm.path),
                            None,
                            Some(i as u32),
                            span,
                            detail,
                        ));
                    }
                }
            }
        }
    }

    /// Paths and verbs a declaration's annotations contribute through reflection chains.
    fn route_parts(
        &self,
        language: Language,
        annotations: &[trace_syntax::boundary::Annotation],
        roots: &[ChannelRow],
        sources: &[&[u8]],
        compiled: &BTreeMap<String, trace_library::channels::ReflectionChain>,
    ) -> RouteParts {
        let mut parts = RouteParts::default();
        for a in annotations {
            let chain = compiled.get(&a.name).cloned();
            let Some(chain) = chain.or_else(|| annotation_chain(language, sources, &a.name, roots)) else {
                continue;
            };
            let Some(root) = roots
                .iter()
                .find(|r| r.symbol.as_deref().is_some_and(|s| simple_name(s) == chain.root))
            else {
                continue;
            };
            if chain.annotation != chain.root {
                parts.derived = true;
            }
            parts
                .via
                .get_or_insert_with(|| root.symbol.clone().unwrap_or_default());
            // Path: the chain's path elements, the root's key element, or the positional value.
            if let Some(key) = &root.key {
                let key_name = match key {
                    ArgSel::Kw(k) | ArgSel::PosOrKw(_, k) => Some(k.as_str()),
                    _ => None,
                };
                for (k, v) in &a.elements {
                    let hit = match k {
                        None => true,
                        Some(k) => chain.path_elements.iter().any(|p| p == k) || Some(k.as_str()) == key_name,
                    };
                    if hit && (v.has_text() || v.plain().is_some()) {
                        parts.paths.push(v.clone());
                    }
                }
                if a.elements.is_empty() && chain.annotation != chain.root {
                    // A composed annotation without elements maps to the root path "".
                    parts.paths.push(Tpl::literal(""));
                }
            }
            // Verb: fixed by the chain, or an element named by the root's verb selector, or
            // (a verb-only root such as a method designator) the annotation's own name.
            if let Some(v) = &chain.verb {
                parts.verbs.extend(verb_of(v));
            } else if let Some(VerbSel::Arg(ArgSel::Kw(k))) = &root.verb {
                let mut found = false;
                for (n, (ek, ev)) in a.elements.iter().enumerate() {
                    if ek.as_deref() == Some(k.as_str()) {
                        let text = ev.plain().or_else(|| a.texts.get(n).cloned()).unwrap_or_default();
                        let verbs: Vec<String> = text.split_whitespace().filter_map(verb_of).collect();
                        found |= !verbs.is_empty();
                        parts.verbs.extend(verbs);
                    }
                }
                if !found && root.key.is_none() {
                    parts.verbs.extend(verb_of(&simple_name(&a.name)));
                }
            }
        }
        parts.paths.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        parts.paths.dedup();
        parts.verbs.sort();
        parts.verbs.dedup();
        parts
    }
}

/// Paths / verbs from one declaration's annotations.
#[derive(Clone, Debug, Default)]
struct RouteParts {
    paths: Vec<Tpl>,
    verbs: Vec<String>,
    derived: bool,
    via: Option<String>,
}

/// Whether an annotation spelling names the row symbol (simple names; C# attribute classes
/// are written without their `Attribute` suffix - the language's attribute naming rule).
fn same_annotation(language: Language, written: &str, symbol: &str) -> bool {
    let w = simple_name(written);
    let s = simple_name(symbol);
    if w == s {
        return true;
    }
    language == Language::CSharp
        && (s.strip_suffix("Attribute") == Some(w.as_str())
            || w.strip_suffix("Attribute") == Some(s.as_str()))
}

#[cfg(test)]
#[path = "../../tests/unit/recognize/reflect.rs"]
mod tests;
