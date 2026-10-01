//! Declared-receiver rules (SPEC 7.10): calls through dereference, wrappers and `super`
//! (child of [`crate::flow`]).

use super::*;

/// Super keyword of a language when used as a receiver (`super.m()`, C# `base.m()`, PHP
/// `parent::m()`). Python's `super()` is a call (language rule, `Effect::Receiver`).
pub(super) fn super_keyword(language: Language) -> Option<&'static str> {
    rules(language).super_receiver.map(|(keyword, _)| keyword)
}

/// The `super(..)` call of a language whose super is a call (Python), from its library
/// adapter (`AdapterSpec::super_call`).
pub(super) fn super_call(language: Language) -> Option<&'static str> {
    trace_library::languages::adapter(language).and_then(|a| a.super_call)
}

/// Receiver part of a method-call callee spelling (the extracted `CallSite::callee` fact,
/// not source text): the spelling before the member and its separator, with the separator
/// (`(**self)` / `.` of `(**self).matched`, `parent` / `::` of `parent::get`).
fn receiver_spelling<'c>(callee: &'c str, member: &str) -> Option<(&'c str, &'static str)> {
    let head = callee.trim_end().strip_suffix(member)?;
    for sep in ["?.", "::", "->", "."] {
        if let Some(r) = head.strip_suffix(sep) {
            return Some((r.trim(), sep));
        }
    }
    None
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

fn balanced(s: &str) -> bool {
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

/// A receiver spelling reduced to (dereferences, root identifier): `(**self)` -> (2, self),
/// `(*x)` -> (1, x), `&mut s` -> (0, s), `x` -> (0, x); anything else (calls, fields,
/// indexing) -> `None`.
fn peel_receiver(text: &str) -> Option<(u32, &str)> {
    let mut t = text.trim();
    let mut derefs = 0u32;
    for _ in 0..16 {
        if let Some(inner) = t.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            if !balanced(inner) {
                return None;
            }
            t = inner.trim();
        } else if let Some(inner) = t.strip_prefix('*') {
            derefs += 1;
            t = inner.trim_start();
        } else if let Some(inner) = t.strip_prefix('&') {
            let inner = inner.trim_start();
            t = inner.strip_prefix("mut ").map_or(inner, str::trim_start);
        } else {
            break;
        }
    }
    is_identifier(t).then_some((derefs, t))
}

/// Candidates the language defines for calls whose receiver is not a tracked value (SPEC
/// 7.10 "Calls through dereference and wrappers"): inferred `flow` `call` candidates for
/// blind unresolved calls (`no_semantic_target` / `no_semantic_backend` /
/// `unresolved_signature` at the callee span), merged into their `no_target` sites like
/// value-flow candidates. Never CHA widening: exactly one member per call.
///
/// * Rust, receiver `self` inside `impl Trait for Type` (Rust trait): through a
///   dereference (`(**self).m()`, `(*self).m()`) or when `Type` is not a type of the index
///   (a generic parameter: `impl<S: Trait + ?Sized> Trait for &mut S`), a method `m` of the
///   trait is `Trait::m` (the bound's method).
/// * Rust, receiver a local / parameter `x` (also `*x`, `(&mut x)`) with a declared type
///   (`FileFacts::types`): `dyn T` / `impl T` / `&dyn T` / `T + U` naming a Rust trait with
///   method `m`; `Box` / `Rc` / `Arc` / `Pin` whose type spelling contains a type reference
///   to such a trait (`Box<dyn T>`); a generic parameter (a type the index does not
///   declare) bounded, in the function header or the enclosing impl header, by exactly one
///   trait with method `m` (`fn f<S: T>(x: S)`, `where S: T`).
/// * Every language with a super keyword ([`super_keyword`]) or a super call
///   ([`super_call`], Python `super()`):
///   `super.m()` / `base.m()` / `parent::m()` / `super().m()` inside a method of class `K`
///   -> the first `m` along `K`'s bases (MRO order).
pub(crate) fn receiver_rule_candidates(index: &Index, hierarchy: &Hierarchy) -> Vec<FlowCandidate> {
    let blind: HashSet<(FileId, ByteSpan)> = index
        .unresolved
        .iter()
        .filter(|u| u.kind.is_blind())
        .map(|u| (u.at.file, u.at.bytes))
        .collect();
    let mut out = Vec::new();
    if blind.is_empty() {
        return out;
    }
    let mut traits: Option<TraitReceivers<'_>> = None;
    for (fi, record) in index.files.iter().enumerate() {
        let Some(facts) = &record.facts else { continue };
        let file = FileId(fi as u32);
        let trait_impls = rules(record.language).trait_impls;
        let super_call = super_call(record.language);
        for c in &facts.calls {
            if !blind.contains(&(file, c.callee_span)) {
                continue;
            }
            let Some(member) = c.member.as_deref() else {
                continue;
            };
            let Some(owner) = facts.executing_owner(c.owner).and_then(|d| record.symbol_of_decl(d)) else {
                continue;
            };
            let target = if trait_impls {
                traits
                    .get_or_insert_with(|| TraitReceivers::new(index, hierarchy))
                    .target(file, facts, c, owner, member)
            } else {
                super_target(index, hierarchy, record.language, super_call, c, owner, member)
            };
            let Some(t) = target.filter(|&t| t != owner) else {
                continue;
            };
            out.push(FlowCandidate {
                owner,
                file,
                span: c.callee_span,
                line: c.line,
                callee: c.callee.clone(),
                candidates: vec![t],
                field_only: Vec::new(),
                test_only: Vec::new(),
                operation: SiteOperation::Call,
                kind: CandidateKind::Flow,
                via: None,
                receiver_exact: false,
                bounded: false,
            });
        }
    }
    out
}

/// The method (the owner or an enclosing callable) that belongs to a class.
fn enclosing_method(index: &Index, hierarchy: &Hierarchy, owner: SymbolId) -> Option<SymbolId> {
    let mut s = owner;
    for _ in 0..32 {
        if hierarchy.class_of(index, s).is_some() {
            return Some(s);
        }
        let p = index.symbol(s).parent?;
        if !index.symbol(p).kind.is_callable() {
            return None;
        }
        s = p;
    }
    None
}

/// Base member a super call reaches (see [`receiver_rule_candidates`]).
fn super_target(
    index: &Index,
    hierarchy: &Hierarchy,
    language: Language,
    super_call: Option<&str>,
    c: &trace_core::facts::CallSite,
    owner: SymbolId,
    member: &str,
) -> Option<SymbolId> {
    let method = enclosing_method(index, hierarchy, owner)?;
    let name: &str = {
        let (recv, sep) = receiver_spelling(&c.callee, member)?;
        let recv = recv.split('<').next().unwrap_or(recv).trim();
        let is_super = match (super_call, rules(language).super_receiver) {
            (Some(call), _) => {
                sep == "."
                    && recv.strip_prefix(call).is_some_and(|args| args.starts_with('('))
                    && recv.ends_with(')')
            }
            (None, Some((keyword, separator))) => sep == separator && recv == keyword,
            (None, None) => false,
        };
        if !is_super {
            return None;
        }
        member
    };
    let class = hierarchy.class_of(index, method)?;
    hierarchy
        .mro(class)
        .into_iter()
        .skip(1)
        .find_map(|k| hierarchy.method(k, name))
        .filter(|&m| m != method)
}

/// Receiver rules of languages whose methods implement traits through impls
/// (`LanguageRules::trait_impls`: Rust; see [`receiver_rule_candidates`]).
struct TraitReceivers<'i> {
    pub(super) index: &'i Index,
    pub(super) hierarchy: &'i Hierarchy,
    /// (file, relation index) -> resolved trait of `FileFacts::impls`.
    traits: HashMap<(FileId, usize), SymbolId>,
    /// Rust trait name -> traits.
    by_name: HashMap<&'i str, Vec<SymbolId>>,
    /// Names of Rust types declared in the index (structs, enums, unions, traits).
    pub(super) types: HashSet<&'i str>,
    /// (file, identifier span) -> semantic value-reference target.
    pub(super) refs: HashMap<(FileId, ByteSpan), SymbolId>,
}

impl<'i> TraitReceivers<'i> {
    pub(super) fn new(index: &'i Index, hierarchy: &'i Hierarchy) -> TraitReceivers<'i> {
        let mut by_name: HashMap<&str, Vec<SymbolId>> = HashMap::new();
        let mut types: HashSet<&str> = HashSet::new();
        for s in &index.symbols {
            if !rules(s.language).trait_impls || !s.kind.is_type() {
                continue;
            }
            types.insert(s.name.as_str());
            if s.kind == trace_core::SymbolKind::Interface {
                by_name.entry(s.name.as_str()).or_default().push(s.id);
            }
        }
        let refs = index
            .value_refs
            .iter()
            .map(|r| ((r.at.file, r.at.bytes), r.target))
            .collect();
        TraitReceivers {
            index,
            hierarchy,
            traits: crate::family::impl_traits(index, hierarchy),
            by_name,
            types,
            refs,
        }
    }

    /// The Rust trait `name` denotes at `span` (a compiler fact there), else the unique
    /// Rust trait of that name.
    fn trait_named(&self, file: FileId, name: &str, span: Option<ByteSpan>) -> Option<SymbolId> {
        let list = self.by_name.get(name)?;
        if let Some(t) = span.and_then(|s| self.refs.get(&(file, s))) {
            if list.contains(t) {
                return Some(*t);
            }
        }
        (list.len() == 1).then(|| list[0])
    }

    /// Traits with a method `member` named by type references inside `within`.
    fn bound_traits(
        &self,
        file: FileId,
        facts: &trace_core::facts::FileFacts,
        within: ByteSpan,
        member: &str,
    ) -> BTreeSet<SymbolId> {
        facts
            .references
            .iter()
            .filter(|r| r.kind == trace_core::facts::RefKind::Type && within.encloses(r.span))
            .filter_map(|r| self.trait_named(file, &r.name, Some(r.span)))
            .filter(|&t| self.hierarchy.method(t, member).is_some())
            .collect()
    }

    /// Innermost out-of-line relation (`impl ... for ...`) of `file` enclosing `span`.
    fn relation(
        facts: &'i trace_core::facts::FileFacts,
        span: ByteSpan,
    ) -> Option<(usize, &'i trace_core::facts::ImplRelation)> {
        facts
            .impls
            .iter()
            .enumerate()
            .filter(|(_, r)| r.span.encloses(span))
            .min_by_key(|(_, r)| r.span.len())
    }

    /// Header of a relation: from its start to the first declaration inside it.
    fn relation_header(&self, file: FileId, rel: &trace_core::facts::ImplRelation) -> ByteSpan {
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

    pub(super) fn target(
        &self,
        file: FileId,
        facts: &'i trace_core::facts::FileFacts,
        c: &trace_core::facts::CallSite,
        owner: SymbolId,
        member: &str,
    ) -> Option<SymbolId> {
        let index = self.index;
        let (recv, sep) = receiver_spelling(&c.callee, member)?;
        if sep != "." {
            return None;
        }
        let (derefs, root) = peel_receiver(recv)?;
        let method = index.symbol(owner);
        if root == "self" {
            let (ri, rel) = Self::relation(facts, method.span.bytes)?;
            let tr = *self.traits.get(&(file, ri))?;
            if index.symbol(tr).kind != trace_core::SymbolKind::Interface {
                return None;
            }
            let tm = self.hierarchy.method(tr, member)?;
            // Through a dereference, or on a type the index does not declare (a generic
            // parameter), the method comes from the trait bound.
            let self_type = crate::hierarchy::base_name(&rel.type_name);
            return (derefs > 0 || !self.types.contains(self_type)).then_some(tm);
        }
        // A local / parameter with a declared type, in the owner or an enclosing callable.
        let mut fact = None;
        let mut scope = Some(owner);
        for _ in 0..16 {
            let Some(s) = scope else { break };
            let subject = trace_core::facts::TypeSubject::Var {
                scope: Scope::Decl(index.symbol(s).decl),
                name: root.to_string(),
            };
            if let Some(t) = facts.types.iter().find(|t| t.subject == subject) {
                fact = Some(t);
                break;
            }
            scope = index.symbol(s).parent.filter(|p| index.symbol(*p).kind.is_callable());
        }
        let fact = fact?;
        let mut found: BTreeSet<SymbolId> = BTreeSet::new();
        for part in fact.type_name.split('+') {
            let base = crate::hierarchy::base_name(part);
            if base.is_empty() {
                continue;
            }
            if let Some(t) = self.trait_named(file, base, None) {
                // `dyn T`, `impl T`, `&dyn T`.
                if self.hierarchy.method(t, member).is_some() {
                    found.insert(t);
                }
            } else if rules(method.language).deref_wrappers.contains(&base) {
                // `Box<dyn T>`: the trait is a type reference inside the spelling.
                found.extend(self.bound_traits(file, facts, fact.span, member));
            } else if !self.types.contains(base) && is_identifier(base) {
                // A generic parameter: its bounds in the function / impl header.
                let header =
                    ByteSpan::new(method.span.bytes.start, method.body_start.max(method.span.bytes.start));
                found.extend(self.bound_traits(file, facts, header, member));
                if let Some((_, rel)) = Self::relation(facts, method.span.bytes) {
                    let header = self.relation_header(file, rel);
                    found.extend(self.bound_traits(file, facts, header, member));
                }
            }
        }
        match found.len() {
            1 => found
                .into_iter()
                .next()
                .and_then(|t| self.hierarchy.method(t, member)),
            _ => None,
        }
    }
}
