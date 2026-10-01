//! Class hierarchy over symbols: bases, methods, MRO, implementations and construction
//! evidence.
//!
//! * classes: `SymbolKind::Class | Interface`;
//! * methods of a class: callables whose `parent` is the class, or whose `container` names a
//!   class (Rust impl, Go receiver, Haskell instance members, C++ out-of-line
//!   `A::f`) resolved among the types of the same language family in order: same file, same
//!   directory, unique globally; so a member declared in any extension / impl block of
//!   a type, in any file, belongs to that type; anonymous scopes (`<lambda>`, `<genexpr>`:
//!   names starting with `<`) are never methods and never join the by-name candidate pool;
//! * bases: each base spelling reduced to its last dotted segment without generic
//!   arguments (`pkg.Base[T]` -> `Base`), resolved to every class with that name in the
//!   same language family, plus `ImplRelation`s from syntax facts;
//! * children: reverse of bases (transitive closure = family);
//! * mro: breadth-first over bases starting at the class;
//! * implementations(stub): nominal descendants of the stub's class having a non-stub method
//!   with the stub's name, plus — when the class has a `Protocol*` base or is an
//!   `Interface` — structural conformers: classes whose method-name set contains every
//!   non-dunder method name of the protocol;
//!   plus the members that implement it outside the class's family (`declared`, same rules
//!   as the family edges of `crate::family`): members declared inside an out-of-line
//!   relation block for the stub's type (`impl Trait for T`, `extension T: P`, Haskell
//!   `instance C T`; also for types outside the index, e.g. `instance C Int`), naming-convention
//!   methods `g.<class>` of a generic `g` ([`convention_methods`], R S3) and the implementors a server reported
//!   (`implements` / `overrides` edges with resolution `implementation`);
//! * library_members: members of repository types whose base / implemented type is declared
//!   outside the index, keyed `<base>.<member>` (the family fallback of library-declared
//!   dispatch for servers without `textDocument/implementation`);
//! * constructed(class): owners of non-constructor edges into the class or into its
//!   constructor (`__init__`/`constructor`), used by evidence cards.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use trace_core::languages::same_module_namespace;
use trace_core::{EdgeKind, FileId, Index, Language, Symbol, SymbolId, SymbolKind};
use trace_syntax::language_rules::rules;

use trace_core::relpath;

pub struct Hierarchy {
    /// class -> method name -> method symbol.
    pub methods: HashMap<SymbolId, HashMap<String, SymbolId>>,
    /// class -> resolved base classes.
    pub bases: HashMap<SymbolId, Vec<SymbolId>>,
    /// class -> direct subclasses.
    pub children: HashMap<SymbolId, Vec<SymbolId>>,
    /// function name -> callable symbols (candidate pool for no_target sites).
    pub functions: HashMap<String, Vec<SymbolId>>,
    /// class -> symbols that construct it.
    pub constructed: HashMap<SymbolId, HashSet<SymbolId>>,
    /// method -> owning class.
    owner: HashMap<SymbolId, SymbolId>,
    /// class -> language-level constructor declarations (`SymbolKind::Constructor`).
    constructors: HashMap<SymbolId, Vec<SymbolId>>,
    /// stub / base member -> members implementing it outside the nominal family (relation
    /// blocks, S3 methods, server implementations).
    declared: HashMap<SymbolId, Vec<SymbolId>>,
    /// `<base>.<member>` of a base / implemented type declared outside the index ->
    /// repository members of that name (sorted by uid).
    pub library_members: HashMap<String, Vec<SymbolId>>,
    /// Bodiless callables whose body is across a language boundary (a `Uses` boundary fact
    /// names the declaration: Java `native` methods): they run, unlike abstract members.
    foreign_bodies: HashSet<SymbolId>,
}

/// Dispatch by naming convention (a language rule, `LanguageRules::generic_method_rule`: R
/// S3): a function `g.<class>` (non-empty class part) is a method of the generic `g`: a
/// function whose body dispatches with a generic dispatch call (R `UseMethod`, declared as a
/// stub by the syntax facts). The longest generic name wins
/// (`as.data.frame.tbl` belongs to `as.data.frame`, not to `as`); several generics of that
/// name: the one in the method's directory, else none. Returns (method, generic) pairs
/// sorted by method id.
pub(crate) fn convention_methods(index: &Index) -> Vec<(SymbolId, SymbolId)> {
    let convention_function = |s: &Symbol| {
        !rules(s.language).generic_method_rule.is_empty()
            && s.kind.is_callable()
            && !is_anonymous(s)
            && !s.parent.is_some_and(|p| index.symbol(p).kind.is_callable())
    };
    let mut generics: HashMap<&str, Vec<SymbolId>> = HashMap::new();
    for s in &index.symbols {
        if convention_function(s) && s.is_stub {
            generics.entry(s.name.as_str()).or_default().push(s.id);
        }
    }
    let mut out = Vec::new();
    if generics.is_empty() {
        return out;
    }
    for s in &index.symbols {
        if !convention_function(s) || s.is_stub {
            continue;
        }
        // Longest generic prefix first: split at the dots from the right.
        let dots: Vec<usize> = s.name.match_indices('.').map(|(i, _)| i).collect();
        for &i in dots.iter().rev() {
            let (prefix, class) = (&s.name[..i], &s.name[i + 1..]);
            if prefix.is_empty() || class.is_empty() {
                continue;
            }
            let Some(candidates) = generics.get(prefix) else {
                continue;
            };
            let dir = relpath::parent(index.file_path(s.file));
            let near: Vec<SymbolId> = candidates
                .iter()
                .copied()
                .filter(|&g| relpath::parent(index.file_path(index.symbol(g).file)) == dir)
                .collect();
            let generic = match (near.as_slice(), candidates.as_slice()) {
                ([only], _) => Some(*only),
                ([], [only]) => Some(*only),
                _ => None,
            };
            if let Some(g) = generic.filter(|&g| g != s.id) {
                out.push((s.id, g));
            }
            break;
        }
    }
    out
}

/// Constructor method name for languages whose constructors are ordinary named methods.
pub(crate) fn constructor_name(language: Language) -> Option<&'static str> {
    rules(language).named_constructor
}

/// True for constructor declarations (`__init__`, JS/TS `constructor`, or a
/// language-level constructor kind).
pub fn is_constructor(symbol: &Symbol) -> bool {
    symbol.kind == SymbolKind::Constructor
        || constructor_name(symbol.language).is_some_and(|n| n == symbol.name)
}

/// Anonymous scope symbols (lambdas, generator expressions) are named `<...>`.
pub(crate) fn is_anonymous(symbol: &Symbol) -> bool {
    symbol.name.starts_with('<')
}

/// Reduce a base / container spelling to the bare type name:
/// `pkg.Base[T]` -> `Base`, `Base<T>` -> `Base`, `public Base` -> `Base`,
/// `std::fmt::Display` -> `Display`, `*Server` -> `Server`. Keyword spellings
/// (`metaclass=ABCMeta`) yield an empty name.
pub fn base_name(spelling: &str) -> &str {
    let s = spelling.trim();
    if s.contains('=') {
        return "";
    }
    let s = s.split(['[', '<', '(', '{']).next().unwrap_or(s).trim();
    let s = s.rsplit(char::is_whitespace).next().unwrap_or(s);
    let s = s.rsplit("::").next().unwrap_or(s);
    let s = s.rsplit('.').next().unwrap_or(s);
    s.trim_matches(|c: char| matches!(c, '*' | '&' | '(' | ')' | ',' | ':'))
}

fn push_base(bases: &mut HashMap<SymbolId, Vec<SymbolId>>, class: SymbolId, base: SymbolId) {
    if class != base {
        let list = bases.entry(class).or_default();
        if !list.contains(&base) {
            list.push(base);
        }
    }
}

impl Hierarchy {
    /// Update after an incremental link (PLAN decision 13): equal to [`Hierarchy::build`] on
    /// the new index. Every map is keyed by dense symbol ids, which an incremental link
    /// shifts for every symbol after the first changed file (`IdRemap`), and a type name
    /// declared in a changed file changes the resolution of every base / container spelled
    /// with it anywhere; re-keying the unchanged entries costs as much as the linear
    /// construction, so the update constructs the hierarchy of the new index.
    pub fn update(
        &mut self,
        index: &Index,
        _remap: &trace_core::delta::IdRemap,
        _delta: &trace_core::delta::IndexDelta,
    ) {
        *self = Hierarchy::build(index);
    }

    pub fn build(index: &Index) -> Hierarchy {
        // Classes by bare name.
        let mut class_names: HashMap<&str, Vec<SymbolId>> = HashMap::new();
        for s in &index.symbols {
            if s.kind.is_type() {
                class_names.entry(s.name.as_str()).or_default().push(s.id);
            }
        }

        // Resolve a type spelling relative to `file` among the types of the file's language
        // family: same file, same directory, unique.
        let resolve_local = |name: &str, file: FileId| -> Option<SymbolId> {
            let language = index.file(file).language;
            let all: Vec<SymbolId> = class_names
                .get(name)?
                .iter()
                .copied()
                .filter(|&c| same_module_namespace(index.symbol(c).language, language))
                .collect();
            if let Some(&c) = all.iter().find(|&&c| index.symbol(c).file == file) {
                return Some(c);
            }
            let dir = relpath::parent(index.file_path(file));
            let same_dir: Vec<SymbolId> = all
                .iter()
                .copied()
                .filter(|&c| relpath::parent(index.file_path(index.symbol(c).file)) == dir)
                .collect();
            if same_dir.len() == 1 {
                return Some(same_dir[0]);
            }
            (all.len() == 1).then(|| all[0])
        };

        // Method ownership.
        let mut owner: HashMap<SymbolId, SymbolId> = HashMap::new();
        for s in &index.symbols {
            if !s.kind.is_callable() || is_anonymous(s) {
                continue;
            }
            let class = match s.parent {
                Some(p) if index.symbol(p).kind.is_type() => Some(p),
                Some(_) => None,
                None => s
                    .container
                    .as_deref()
                    .map(base_name)
                    .filter(|n| !n.is_empty())
                    .and_then(|n| resolve_local(n, s.file)),
            };
            if let Some(c) = class {
                owner.insert(s.id, c);
            }
        }

        // Methods per class; later declarations win (redefinitions), as in a Python dict.
        let mut methods: HashMap<SymbolId, HashMap<String, SymbolId>> = HashMap::new();
        let mut constructors: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        for s in &index.symbols {
            if let Some(&c) = owner.get(&s.id) {
                methods.entry(c).or_default().insert(s.name.clone(), s.id);
                if s.kind == SymbolKind::Constructor {
                    constructors.entry(c).or_default().push(s.id);
                }
            }
        }

        // Callables by name (candidate pool), sorted by uid.
        let mut functions: HashMap<String, Vec<SymbolId>> = HashMap::new();
        for s in &index.symbols {
            if s.kind.is_callable() && !is_anonymous(s) {
                functions.entry(s.name.clone()).or_default().push(s.id);
            }
        }
        for list in functions.values_mut() {
            list.sort_by(|a, b| index.symbol(*a).uid.cmp(&index.symbol(*b).uid));
        }

        // Bases: spelled bases resolved by name, plus out-of-line impl relations.
        let mut bases: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        for s in &index.symbols {
            if !s.kind.is_type() {
                continue;
            }
            for spelling in &s.bases {
                let name = base_name(spelling);
                if name.is_empty() {
                    continue;
                }
                for &b in class_names.get(name).map(Vec::as_slice).unwrap_or(&[]) {
                    // Types of another language family are never bases.
                    if same_module_namespace(index.symbol(b).language, s.language) {
                        push_base(&mut bases, s.id, b);
                    }
                }
            }
        }
        for (fi, file) in index.files.iter().enumerate() {
            let Some(facts) = &file.facts else { continue };
            let fid = FileId(fi as u32);
            for rel in &facts.impls {
                let type_name = base_name(&rel.type_name);
                let trait_name = base_name(&rel.trait_name);
                if type_name.is_empty() || trait_name.is_empty() {
                    continue;
                }
                let Some(ty) = resolve_local(type_name, fid) else {
                    continue;
                };
                let language = file.language;
                for &t in class_names.get(trait_name).map(Vec::as_slice).unwrap_or(&[]) {
                    if same_module_namespace(index.symbol(t).language, language) {
                        push_base(&mut bases, ty, t);
                    }
                }
            }
        }

        let mut children: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        let mut ordered: Vec<(&SymbolId, &Vec<SymbolId>)> = bases.iter().collect();
        ordered.sort_by_key(|(c, _)| **c);
        for (&c, bs) in ordered {
            for &b in bs {
                children.entry(b).or_default().push(c);
            }
        }

        // Construction evidence. Reference edges did not exist
        // in the reference design (value references were not edges) and are not counted.
        let mut constructed: HashMap<SymbolId, HashSet<SymbolId>> = HashMap::new();
        for e in &index.edges {
            // Unchanged from the reference design for the original kinds (passes_callback
            // still counts, so evidence hashes of cached decisions stay stable); the non-call
            // kinds added in schema 5 never count.
            if matches!(
                e.kind,
                EdgeKind::Constructor
                    | EdgeKind::References
                    | EdgeKind::Writes
                    | EdgeKind::Imports
                    | EdgeKind::Reexports
                    | EdgeKind::Overrides
                    | EdgeKind::Implements
                    | EdgeKind::Bridge
            ) {
                continue;
            }
            let target = index.symbol(e.to);
            if target.kind.is_type() {
                constructed.entry(e.to).or_default().insert(e.from);
            } else if is_constructor(target) {
                if let Some(&c) = owner.get(&e.to) {
                    constructed.entry(c).or_default().insert(e.from);
                }
            }
        }

        // Implementations outside the nominal family, and members of library-based types.
        let mut declared: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        let mut library_members: HashMap<String, Vec<SymbolId>> = HashMap::new();
        let has_type = |name: &str, language: Language| {
            class_names.get(name).is_some_and(|v| {
                v.iter()
                    .any(|&c| same_module_namespace(index.symbol(c).language, language))
            })
        };
        let member_like = |s: &Symbol| {
            s.kind.is_callable() && !is_anonymous(s) && !is_constructor(s) && s.kind != SymbolKind::Module
        };
        for (fi, file) in index.files.iter().enumerate() {
            let Some(facts) = &file.facts else { continue };
            let fid = FileId(fi as u32);
            for rel in &facts.impls {
                let type_name = base_name(&rel.type_name);
                let trait_name = base_name(&rel.trait_name);
                if type_name.is_empty() || trait_name.is_empty() {
                    continue;
                }
                let members: Vec<&Symbol> = index
                    .symbols_of(fid)
                    .iter()
                    .filter(|s| {
                        member_like(s)
                            && rel.span.encloses(s.span.bytes)
                            && s.container.as_deref().map(base_name) == Some(type_name)
                    })
                    .collect();
                if members.is_empty() {
                    continue;
                }
                if !has_type(trait_name, file.language) {
                    for m in &members {
                        library_members
                            .entry(format!("{trait_name}.{}", m.name))
                            .or_default()
                            .push(m.id);
                    }
                    continue;
                }
                for &t in class_names.get(trait_name).map(Vec::as_slice).unwrap_or(&[]) {
                    if !same_module_namespace(index.symbol(t).language, file.language) {
                        continue;
                    }
                    for m in &members {
                        if let Some(base) = methods.get(&t).and_then(|ms| ms.get(&m.name)) {
                            if *base != m.id {
                                declared.entry(*base).or_default().push(m.id);
                            }
                        }
                    }
                }
            }
        }
        for s in &index.symbols {
            if !s.kind.is_type() || s.is_synthetic() {
                continue;
            }
            let external: Vec<&str> = s
                .bases
                .iter()
                .map(|b| base_name(b))
                .filter(|n| !n.is_empty() && !has_type(n, s.language))
                .collect();
            if external.is_empty() {
                continue;
            }
            let Some(own) = methods.get(&s.id) else { continue };
            for (name, &m) in own {
                if !member_like(index.symbol(m)) {
                    continue;
                }
                for base in &external {
                    library_members.entry(format!("{base}.{name}")).or_default().push(m);
                }
            }
        }
        for (method, generic) in convention_methods(index) {
            declared.entry(generic).or_default().push(method);
        }
        for e in &index.edges {
            if e.resolution == trace_core::Resolution::Implementation
                && matches!(e.kind, EdgeKind::Implements | EdgeKind::Overrides)
                && e.from != e.to
            {
                declared.entry(e.to).or_default().push(e.from);
            }
        }
        for list in declared.values_mut().chain(library_members.values_mut()) {
            list.sort_by(|a, b| index.symbol(*a).uid.cmp(&index.symbol(*b).uid));
            list.dedup();
        }

        let mut foreign_bodies: HashSet<SymbolId> = HashSet::new();
        for record in &index.files {
            let Some(facts) = record.facts.as_ref() else { continue };
            for b in &facts.boundaries {
                if b.role != trace_core::facts::BoundaryRole::Uses {
                    continue;
                }
                if let Some(m) = b.decl.and_then(|d| record.symbol_of_decl(d)) {
                    let sym = index.symbol(m);
                    if sym.is_stub && sym.kind.is_callable() {
                        foreign_bodies.insert(m);
                    }
                }
            }
        }

        Hierarchy {
            methods,
            bases,
            children,
            functions,
            constructed,
            owner,
            constructors,
            declared,
            library_members,
            foreign_bodies,
        }
    }

    /// Whether member `m` runs nothing when dispatched to: a stub (abstract / interface
    /// member, prototype) whose body is not across a language boundary.
    pub(crate) fn runs_nothing(&self, index: &Index, m: SymbolId) -> bool {
        index.symbol(m).is_stub && !self.foreign_bodies.contains(&m)
    }

    /// Repository members implementing the library-declared `<base>.<member>` that a
    /// library symbol names (`pkg.mod.Base.member`, `pkg::Base::member`, `Base#member`):
    /// its last two segments.
    pub(crate) fn library_implementations(&self, symbol: &str) -> &[SymbolId] {
        if self.library_members.is_empty() {
            return &[];
        }
        let normalized = symbol.replace("::", ".").replace(['#', '/', ':'], ".");
        let parts: Vec<&str> = normalized.split('.').filter(|p| !p.is_empty()).collect();
        match parts.as_slice() {
            [.., base, member] => self
                .library_members
                .get(&format!("{base}.{member}"))
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            _ => &[],
        }
    }

    /// Owning class of a method, if any.
    pub fn class_of(&self, _index: &Index, method: SymbolId) -> Option<SymbolId> {
        self.owner.get(&method).copied()
    }

    /// Language-level constructor declarations of exactly this class (not inherited).
    pub(crate) fn declared_constructors(&self, class: SymbolId) -> &[SymbolId] {
        self.constructors.get(&class).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Method named `name` declared directly on `class`.
    pub fn method(&self, class: SymbolId, name: &str) -> Option<SymbolId> {
        self.methods.get(&class)?.get(name).copied()
    }

    /// The class and all transitive subclasses.
    pub fn family(&self, class: SymbolId) -> HashSet<SymbolId> {
        let mut seen = HashSet::from([class]);
        let mut todo = vec![class];
        while let Some(c) = todo.pop() {
            for &s in self.children.get(&c).map(Vec::as_slice).unwrap_or(&[]) {
                if seen.insert(s) {
                    todo.push(s);
                }
            }
        }
        seen
    }

    /// Breadth-first base order starting at `class`.
    pub fn mro(&self, class: SymbolId) -> Vec<SymbolId> {
        let mut seen: Vec<SymbolId> = Vec::new();
        let mut visited: HashSet<SymbolId> = HashSet::new();
        let mut todo = VecDeque::from([class]);
        while let Some(c) = todo.pop_front() {
            if visited.insert(c) {
                seen.push(c);
                todo.extend(self.bases.get(&c).map(Vec::as_slice).unwrap_or(&[]));
            }
        }
        seen
    }

    /// Concrete implementations for a stub method (sorted by uid): the nominal and
    /// structural members of its class's family plus the members implementing it outside
    /// that family (module docs, `declared`).
    pub fn implementations(&self, index: &Index, stub: SymbolId) -> Vec<SymbolId> {
        let extra: Vec<SymbolId> = self
            .declared
            .get(&stub)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|&m| m != stub && !self.runs_nothing(index, m))
                    .collect()
            })
            .unwrap_or_default();
        let Some(&class) = self.owner.get(&stub) else {
            return extra;
        };
        let name = index.symbol(stub).name.as_str();
        let mut found: BTreeSet<SymbolId> = self.family(class).into_iter().collect();
        found.remove(&class);
        let proto = index.symbol(class);
        let structural = proto.kind == SymbolKind::Interface
            || proto.bases.iter().any(|b| base_name(b).starts_with("Protocol"));
        if structural {
            let required: Vec<&str> = self
                .methods
                .get(&class)
                .map(|m| {
                    m.keys()
                        .map(String::as_str)
                        .filter(|n| !n.starts_with("__"))
                        .collect()
                })
                .unwrap_or_default();
            if !required.is_empty() {
                for (&other, members) in &self.methods {
                    if other != class && required.iter().all(|r| members.contains_key(*r)) {
                        found.insert(other);
                    }
                }
            }
        }
        let mut result: Vec<SymbolId> = found
            .into_iter()
            .filter_map(|c| self.method(c, name))
            .filter(|&m| m != stub && !self.runs_nothing(index, m))
            .collect();
        result.extend(extra);
        result.sort_by(|a, b| index.symbol(*a).uid.cmp(&index.symbol(*b).uid));
        result.dedup();
        result
    }
}

#[cfg(test)]
#[path = "../tests/unit/hierarchy.rs"]
mod tests;
