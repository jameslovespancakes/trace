//! Candidate narrowing for calls without a semantic target (SPEC §7.11, NEXT.md items 16
//! and 20). Syntax facts only (tree-sitter declarations, calls, imports, exports); no source
//! text is read and nothing is ever executed.
//!
//! Every rule is a language rule, applied only where the language defines it and the syntax
//! facts are complete enough; when in doubt a candidate is kept.
//!
//! **Name matching (every blind call, semantic languages included — item 20):**
//! * *Shadowing*: a bare callee bound in the calling function (or an enclosing function) as a
//!   parameter or local variable denotes that value, never a method: method candidates are
//!   dropped in languages where locals shadow callable names (Python, JS/TS, Go, C/C++,
//!   Rust, R, Scala, Haskell). Same-name functions are not
//!   visible either (below): the value comes from value flow.
//! * *Bare calls and methods*: a bare `m()` never reaches a method in languages without an
//!   implicit receiver (Python, JS/TS, Go, C, Rust, PHP, Bash) — except Python class-body
//!   code calling a method of that class; in languages with an implicit receiver (Java, C#,
//!   C++) only methods of the class family (ancestors and subclasses) of a lexically
//!   enclosing class, unless an import can bind the name (Java/C# static imports; files of
//!   those languages without any import facts are not narrowed).
//! * *Lexical nesting*: a function nested in another function is visible to bare calls only
//!   inside that function (not PHP, Bash, whose nested definitions are global).
//! * *Reachability*: a free function in another file is a strong candidate for a bare call
//!   only if that file is reachable under the language's name rules — Python, JS/TS, Rust,
//!   Haskell: an import binds the name (or a wildcard import of that file); Go: same package
//!   (directory). Other candidates are kept but *weak*
//!   (`field_only`-style: possible tier, never decided alone).
//!
//! **Visibility (a declaration the call cannot name is no candidate):**
//! * *Lexical shadowing*: a bare callee bound as a local / parameter of the caller (proven
//!   by `FileFacts::local_spans`, or a parameter / local in a language where locals shadow
//!   callables) denotes that binding: same-name declarations are dropped
//!   (`dropped_by_visibility`); value flow may still deliver the function the local holds.
//! * *Module bindings*: in JavaScript / TypeScript, a top-level binding of another ES module
//!   that the calling module does not import (non-exported bindings never can be) is not
//!   visible (script files without imports / exports share one scope and stay weak).
//!
//! **Rules before anything is possible (general fixes, SPEC §7.11), in this order:**
//! 1. *imports / `use` / `require`*: explicit single-name imports name their declarations
//!    exactly by the import-path rule ([`crate::imports::declarations_at_path`]; Java,
//!    Scala);
//! 2. *package / module*: Go packages are directories; Bash scripts see only their own and
//!    sourced files' functions;
//! 3. *receiver allocation*: a receiver that is itself an allocation (`new K().m()`, Go
//!    `K{..}.m()`) narrows method candidates to that type's family;
//! 4. *class member vs local scope*: a bare callee that `FileFacts::local_spans` proves to be
//!    a local / parameter binding drops method candidates in every language;
//! 5. *arity*: a candidate whose declared parameter list accepts no binding of the call's
//!    plain positional arguments is dropped (`dropped_by_arity`, [`arity_rules_out`];
//!    Python, Java, Rust, PHP).
//!
//! Candidates stay in the calling language's namespace ([`name_interop`]): a declaration of
//! another language is reachable only through a bridge, never by a shared name.
//!
//! Every blind call comes from a file a language server analysed (there is no syntax-only
//! mode): narrowing only removes what the language cannot mean. Never proven.

use std::collections::{HashMap, HashSet};

use trace_core::facts::{ArgSlot, BindTarget, CallDetail, CallSite, Expr, FlowFact, ImportKind, Scope};
use trace_core::relpath;
use trace_core::{FileId, Index, Language, SymbolId, SymbolKind, Unresolved};
use trace_syntax::language_rules::{rules, BareCalls, ModulePaths, NameReach};

use crate::hierarchy::{is_anonymous, Hierarchy};

mod modules;

pub use modules::ModuleMap;

/// Result of narrowing one call's candidate pool.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Narrowing {
    /// Surviving candidates, in pool order (at most `limit`).
    pub kept: Vec<SymbolId>,
    pub dropped_by_import: Vec<SymbolId>,
    pub dropped_by_receiver: Vec<SymbolId>,
    /// Dropped by lexical scope (nested functions, methods behind a missing receiver,
    /// shadowed callee names).
    pub dropped_by_scope: Vec<SymbolId>,
    /// Declarations not visible at the call by name: the callee name is a local /
    /// parameter binding (lexical shadowing), or a top-level binding of another ES module
    /// the calling module does not import. Unlike the other drops, value flow may still
    /// deliver them (the local may hold that function).
    pub dropped_by_visibility: Vec<SymbolId>,
    /// Declarations whose parameter list accepts no binding of the call's positional
    /// arguments ([`arity_rules_out`], rule `arity`).
    pub dropped_by_arity: Vec<SymbolId>,
    /// Kept candidates whose only evidence is the shared name (not reachable under the
    /// language's name rules): possible tier, never decided alone.
    pub weak: Vec<SymbolId>,
    /// More than `limit` candidates survived.
    pub truncated: bool,
}

/// Syntactic form of a callee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallForm {
    /// `m()`; `shadowed`: the name is a parameter / local of the caller (or an enclosing
    /// function); `local`: the syntax facts prove the callee identifier denotes a local /
    /// parameter binding (`FileFacts::local_spans`, every language).
    Bare { shadowed: bool, local: bool },
    /// `x.m()`, `x->m()`, `x:m()`, `x$m()`: `root` = `x` when it is a plain identifier;
    /// `value` = the receiver is certainly a value (self name, parameter/local, or an
    /// expression that is not a plain identifier).
    Receiver { root: Option<String>, value: bool },
    /// `A::m()`, `A\m()`: `root` = first segment of the qualifier.
    Path { root: Option<String> },
    /// Anything else (subscripts, calls of calls): no shape rule applies.
    Other,
}

/// A blind call to narrow.
#[derive(Clone, Debug)]
pub struct CallShape<'i> {
    pub file: FileId,
    pub language: Language,
    pub owner: SymbolId,
    pub call: Option<&'i CallSite>,
    pub detail: Option<&'i CallDetail>,
    /// Called member name (the pool key).
    pub member: String,
    pub form: CallForm,
}

/// Verdict for one candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Keep,
    Weak,
    Import,
    Receiver,
    Scope,
    Visibility,
    Arity,
}

/// Whether code in `caller` can call a declaration written in `callee` by name, without a
/// bridge: the same language, or one shared namespace (JavaScript/TypeScript modules and the
/// script blocks of Vue / Svelte components, the C linker namespace of C/C++, the JVM class
/// namespace of Java/Scala/Clojure, the .NET namespace of C#/F#/Visual Basic/PowerShell).
/// Any other
/// pairing crosses a language boundary, which only a bridge (binding attribute, ABI rule,
/// contract) can connect; a name match alone is never a call candidate, and a name scan
/// counts such an occurrence as `other_language` (SPEC §10.1).
pub fn name_interop(caller: Language, callee: Language) -> bool {
    trace_core::languages::same_family(caller, callee)
}

/// Languages where a parameter / local shadows a callable of the same name.
fn locals_shadow(language: Language) -> bool {
    rules(language).locals_shadow_functions
}

/// Languages whose nested named functions are lexically scoped.
fn nested_functions_are_lexical(language: Language) -> bool {
    !rules(language).global_nested_functions
}

/// An identifier (letters, digits, `_`, `$`; not starting with a digit).
pub(crate) fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c == '$' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c == '$' || c.is_alphanumeric())
}

/// A variable name without its sigil (`$cookie` -> `cookie`).
fn local_name(s: &str) -> &str {
    s.strip_prefix('$').unwrap_or(s)
}

fn is_self_name(s: &str) -> bool {
    matches!(s, "self" | "this" | "$this" | "cls" | "super" | "@")
}

/// Split a callee into (form, qualifier) given its member name.
fn split_callee<'c>(callee: &'c str, member: &str) -> (Option<&'static str>, &'c str) {
    let text = callee.trim();
    let Some(prefix) = text.strip_suffix(member) else {
        return (Some("other"), "");
    };
    let prefix = prefix.trim_end();
    if prefix.is_empty() {
        return (None, "");
    }
    for (sep, kind) in [
        ("::", "path"),
        ("\\", "path"),
        ("?.", "recv"),
        ("&.", "recv"),
        ("->", "recv"),
        (".", "recv"),
        (":", "recv"),
        ("$", "recv"),
    ] {
        if let Some(q) = prefix.strip_suffix(sep) {
            return (Some(kind), q.trim());
        }
    }
    (Some("other"), "")
}

/// First identifier segment of a qualifier (`a::b` -> `a`, `pkg.sub` -> `pkg`).
fn root_segment(qualifier: &str) -> Option<&str> {
    let q = qualifier.trim_start_matches('\\');
    let end = q
        .find(|c: char| !(c == '_' || c == '$' || c.is_alphanumeric()))
        .unwrap_or(q.len());
    let root = &q[..end];
    is_identifier(root).then_some(root)
}

/// Narrowing context over one index (caches per file and per name).
pub(crate) struct Narrower<'i> {
    index: &'i Index,
    hierarchy: &'i Hierarchy,
    modules: Option<ModuleMap>,
    /// file -> (decl, bound name) pairs: parameters (every kind) and local variables.
    bound: HashMap<FileId, HashSet<(u32, String)>>,
    /// (file, callee span start, end) -> call index.
    calls: HashMap<(FileId, u32, u32), u32>,
    /// Implicit-receiver data of the call being narrowed: (owner, member) -> (class family
    /// of the enclosing classes, whether an import may bind the name).
    receiver_cache: Option<((SymbolId, String), HashSet<SymbolId>, bool)>,
    /// Allocated type name -> classes a method call on that allocation may reach (the
    /// types of that name, their ancestors and their subclasses).
    literal_types: HashMap<String, Option<HashSet<SymbolId>>>,
}

impl<'i> Narrower<'i> {
    pub fn new(index: &'i Index, hierarchy: &'i Hierarchy) -> Narrower<'i> {
        let mut calls = HashMap::new();
        for (fi, f) in index.files.iter().enumerate() {
            let Some(facts) = &f.facts else { continue };
            for (ci, c) in facts.calls.iter().enumerate() {
                calls
                    .entry((FileId(fi as u32), c.callee_span.start, c.callee_span.end))
                    .or_insert(ci as u32);
            }
        }
        Narrower {
            index,
            hierarchy,
            modules: None,
            bound: HashMap::new(),
            calls,
            receiver_cache: None,
            literal_types: HashMap::new(),
        }
    }

    /// Classes the receiver of `new K(..).m()` / Go `K{..}.m()` may be (the receiver
    /// expression itself is an allocation of the named type `K`): every type named `K`
    /// with its ancestors (inherited methods) and subclasses (anonymous subclass bodies,
    /// `new K() { .. }`). Only in languages whose allocations name the run-time class and
    /// Go (a composite literal's method set is that of its named type). `None` when the
    /// receiver is anything else, no type of that name is declared, or no candidate of the
    /// pool belongs to those classes (then the method may be promoted or inherited from a
    /// type the index does not relate, and nothing is dropped).
    fn literal_receiver(&mut self, shape: &CallShape<'i>, pool: &[SymbolId]) -> Option<HashSet<SymbolId>> {
        if !rules(shape.language).allocation_receivers {
            return None;
        }
        let receiver = shape.detail?.receiver.as_ref()?;
        let Expr::Call {
            func, is_new: true, ..
        } = receiver
        else {
            return None;
        };
        let name = match func.as_ref() {
            Expr::Name { name, .. } => name.as_str(),
            Expr::Attr { attr, .. } => attr.as_str(),
            _ => return None,
        };
        let name = name.rsplit(['\\', '.', ':']).next().unwrap_or(name);
        if name.is_empty() {
            return None;
        }
        if !self.literal_types.contains_key(name) {
            let index = self.index;
            let mut accept: HashSet<SymbolId> = HashSet::new();
            for s in index.symbols.iter().filter(|s| s.kind.is_type() && s.name == name) {
                accept.extend(self.hierarchy.mro(s.id));
                accept.extend(self.hierarchy.family(s.id));
            }
            let value = (!accept.is_empty()).then_some(accept);
            self.literal_types.insert(name.to_string(), value);
        }
        let accept = self.literal_types.get(name)?.as_ref()?;
        let index = self.index;
        let hierarchy = self.hierarchy;
        let any = pool.iter().any(|&p| {
            p != shape.owner
                && self.is_method(p)
                && hierarchy.class_of(index, p).is_some_and(|k| accept.contains(&k))
        });
        any.then(|| accept.clone())
    }

    /// (class family of the enclosing classes, an import may bind the name) for the call,
    /// computed once per (owner, member).
    fn implicit_receiver(&mut self, shape: &CallShape<'i>) -> (&HashSet<SymbolId>, bool) {
        let key = (shape.owner, shape.member.clone());
        if self.receiver_cache.as_ref().is_none_or(|(k, _, _)| *k != key) {
            let static_imports = rules(shape.language).static_imports;
            let imports = self.visible_imports(shape.file, shape.owner);
            let binds = |i: &&trace_core::facts::Import| {
                i.local == shape.member || i.target.rsplit(['.', ':']).next() == Some(shape.member.as_str())
            };
            let unknown = if static_imports {
                imports.is_empty() || imports.iter().any(|i| binds(i) || i.kind == ImportKind::Wildcard)
            } else {
                false
            };
            let classes = self.enclosing_classes(shape.owner);
            let family = self.families(&classes);
            self.receiver_cache = Some((key, family, unknown));
        }
        let (_, family, unknown) = self.receiver_cache.as_ref().expect("cached");
        (family, *unknown)
    }

    /// The syntax call whose callee span is exactly `span` (indexed lookup).
    pub fn call_at(&self, file: FileId, span: trace_core::ByteSpan) -> Option<(u32, &'i CallSite)> {
        let index: &'i Index = self.index;
        let ci = *self.calls.get(&(file, span.start, span.end))?;
        let facts = index.file(file).facts.as_ref()?;
        facts.calls.get(ci as usize).map(|c| (ci, c))
    }

    fn modules(&mut self) -> &ModuleMap {
        let index = self.index;
        self.modules.get_or_insert_with(|| ModuleMap::new(index))
    }

    /// Names bound as parameters or local variables per declaration of `file`.
    fn bound(&mut self, file: FileId) -> &HashSet<(u32, String)> {
        let index = self.index;
        self.bound.entry(file).or_insert_with(|| {
            let mut set = HashSet::new();
            let Some(facts) = index.file(file).facts.as_ref() else {
                return set;
            };
            for (d, decl) in facts.declarations.iter().enumerate() {
                for p in &decl.parameters {
                    set.insert((d as u32, local_name(&p.name).to_string()));
                }
            }
            for fact in &facts.flow {
                if let FlowFact::Bind {
                    target:
                        BindTarget::Var {
                            scope: Scope::Decl(d),
                            name,
                        },
                    scope: Scope::Decl(s),
                    ..
                } = fact
                {
                    if d == s {
                        set.insert((*d, local_name(name).to_string()));
                    }
                }
            }
            set
        })
    }

    /// Whether `name` is a parameter or local of `owner` or an enclosing callable.
    fn is_bound_locally(&mut self, owner: SymbolId, name: &str) -> bool {
        let index = self.index;
        let mut s = owner;
        loop {
            let sym = index.symbol(s);
            if !sym.kind.is_callable() {
                return false;
            }
            let key = (sym.decl, local_name(name).to_string());
            if self.bound(sym.file).contains(&key) {
                return true;
            }
            match sym.parent {
                Some(p) => s = p,
                None => return false,
            }
        }
    }

    /// Shape of a blind call (`u.owner` must be `owner`).
    pub fn shape(&mut self, u: &Unresolved, owner: SymbolId, member: &str) -> CallShape<'i> {
        let index: &'i Index = self.index;
        let file = u.at.file;
        let language = index.file(file).language;
        let found = self.call_at(file, u.at.bytes);
        let call = found.map(|(_, c)| c);
        let detail = found.and_then(|(ci, _)| {
            index
                .file(file)
                .facts
                .as_ref()
                .and_then(|f| f.call_detail(ci as usize))
        });
        let callee = call.map_or(u.callee.as_str(), |c| c.callee.as_str());
        let at = call.map_or(u.at.bytes, |c| c.callee_span);
        let facts = index.file(file).facts.as_ref();
        let (kind, qualifier) = split_callee(callee, member);
        let form = match kind {
            None => {
                // The callee identifier is a local / parameter binding (every language).
                let local = facts.is_some_and(|f| f.is_local(at));
                CallForm::Bare {
                    shadowed: local || self.is_bound_locally(owner, member),
                    local,
                }
            }
            Some("path") => CallForm::Path {
                root: root_segment(qualifier).map(str::to_string),
            },
            Some("recv") => {
                let plain = is_identifier(qualifier) || is_self_name(qualifier);
                let receiver_local = detail
                    .and_then(|d| d.receiver.as_ref())
                    .and_then(|r| match r {
                        Expr::Name { span, .. } => Some(*span),
                        _ => None,
                    })
                    .is_some_and(|span| facts.is_some_and(|f| f.is_local(span)));
                let value = !plain
                    || is_self_name(qualifier)
                    || qualifier.starts_with('@')
                    || receiver_local
                    || self.is_bound_locally(owner, qualifier);
                CallForm::Receiver {
                    root: plain.then(|| qualifier.to_string()),
                    value,
                }
            }
            _ => CallForm::Other,
        };
        CallShape {
            file,
            language,
            owner,
            call,
            detail,
            member: member.to_string(),
            form,
        }
    }

    /// Whether `file` declares a top-level function `name` and imports (sources) nothing.
    fn defines_own(&self, file: FileId, name: &str) -> bool {
        let rec = self.index.file(file);
        if rec.facts.as_ref().is_none_or(|f| !f.imports.is_empty()) {
            return false;
        }
        self.index
            .symbols_of(file)
            .iter()
            .any(|s| s.parent.is_none() && s.kind.is_callable() && s.name == name)
    }

    /// Files sourced by a Bash script (one hop), or `None` when it sources nothing or any
    /// `source` target does not resolve to repository files.
    fn sourced_files(&mut self, file: FileId) -> Option<Vec<FileId>> {
        let index: &'i Index = self.index;
        let facts = index.file(file).facts.as_ref()?;
        if facts.imports.is_empty() {
            return None;
        }
        let path = index.file_path(file).to_string();
        let language = index.file(file).language;
        let mut out = Vec::new();
        for i in &facts.imports {
            let target = i.target.clone();
            let (files, _) = self.modules().resolve(&path, language, &target, false)?;
            out.extend(files);
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    }

    /// Whether `id` is a method (member of a type, or declared as one).
    fn is_method(&self, id: SymbolId) -> bool {
        let s = self.index.symbol(id);
        s.kind == SymbolKind::Method
            || s.kind == SymbolKind::Constructor
            || self.hierarchy.class_of(self.index, id).is_some()
    }

    /// Function nested in another callable (lexically scoped in most languages).
    fn nesting_function(&self, id: SymbolId) -> Option<SymbolId> {
        let p = self.index.symbol(id).parent?;
        self.index.symbol(p).kind.is_callable().then_some(p)
    }

    fn encloses(&self, outer: SymbolId, inner: SymbolId) -> bool {
        let mut s = Some(inner);
        while let Some(x) = s {
            if x == outer {
                return true;
            }
            s = self.index.symbol(x).parent;
        }
        false
    }

    /// Classes lexically enclosing the owner (and the class of an out-of-line method).
    fn enclosing_classes(&self, owner: SymbolId) -> Vec<SymbolId> {
        let mut out = Vec::new();
        let mut s = Some(owner);
        while let Some(x) = s {
            let sym = self.index.symbol(x);
            if sym.kind.is_type() {
                out.push(x);
            } else if let Some(c) = self.hierarchy.class_of(self.index, x) {
                out.push(c);
            }
            s = sym.parent;
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Ancestors and descendants of the given classes.
    fn families(&self, classes: &[SymbolId]) -> HashSet<SymbolId> {
        let mut out = HashSet::new();
        for &c in classes {
            out.extend(self.hierarchy.mro(c));
            out.extend(self.hierarchy.family(c));
        }
        out
    }

    /// Imports of `file` visible from `owner` (module-level or in an enclosing function).
    fn visible_imports(&self, file: FileId, owner: SymbolId) -> Vec<&'i trace_core::facts::Import> {
        let index: &'i Index = self.index;
        let Some(facts) = index.file(file).facts.as_ref() else {
            return Vec::new();
        };
        facts
            .imports
            .iter()
            .filter(|i| match i.scope {
                Scope::Module => true,
                Scope::Decl(d) => index
                    .file(file)
                    .symbol_of_decl(d)
                    .is_some_and(|s| self.encloses(s, owner)),
            })
            .collect()
    }

    /// Files an import binding `name` (visible from `owner`) refers to: `Some((files,
    /// names_module))`, `None` when no import binds the name or its target does not resolve.
    fn import_files(&mut self, file: FileId, owner: SymbolId, name: &str) -> Option<(Vec<FileId>, bool)> {
        let imports = self.visible_imports(file, owner);
        let import = imports.iter().rev().find(|i| i.local == name)?;
        let language = self.index.file(file).language;
        let path = self.index.file_path(file).to_string();
        let member = import.kind == ImportKind::Member;
        let target = import.target.clone();
        let (mut files, whole) = self.modules().resolve(&path, language, &target, member)?;
        // One re-export hop (`export ... from`, `pub use`, `__all__` imports).
        let extra = self.reexported(&files, name);
        files.extend(extra);
        files.sort_unstable();
        files.dedup();
        Some((files, whole || import.kind == ImportKind::Module))
    }

    /// Files re-exported by `files` under `name` (or wildcard re-exports).
    fn reexported(&mut self, files: &[FileId], name: &str) -> Vec<FileId> {
        let index = self.index;
        let mut out = Vec::new();
        for &f in files {
            let Some(facts) = index.file(f).facts.as_ref() else {
                continue;
            };
            let path = index.file_path(f).to_string();
            let language = index.file(f).language;
            for e in &facts.exports {
                if e.exported != name && e.exported != "*" {
                    continue;
                }
                let member = e.exported != "*";
                if let Some((more, _)) = self.modules().resolve(&path, language, &e.target, member) {
                    out.extend(more);
                }
            }
        }
        out
    }

    /// Java / Scala explicit single-name import rule (see the call site). `None` when
    /// the language has no such rule or no import binds the called name.
    fn explicit_import_verdict(&mut self, shape: &CallShape<'i>, c: SymbolId) -> Option<Verdict> {
        if !rules(shape.language).single_name_imports {
            return None;
        }
        // Members of the implicit receiver (the enclosing class family) take precedence over
        // imported names in Java (class members shadow static imports): those
        // candidates are left to the implicit-receiver rule.
        // An unknown (external) part of the family cannot contain `k`, a repository class:
        // a repository base would be part of the known family.
        if let Some(k) = self.hierarchy.class_of(self.index, c) {
            let (family, _unknown) = self.implicit_receiver(shape);
            if family.contains(&k) {
                return None;
            }
        }
        let imports = self.visible_imports(shape.file, shape.owner);
        let import = imports
            .iter()
            .rev()
            .find(|i| i.kind == ImportKind::Member && i.local == shape.member)?;
        // The import-path rule names the imported declarations exactly (package
        // directories): only they remain.
        let index: &'i Index = self.index;
        let path = index.file_path(shape.file).to_string();
        let named = crate::imports::declarations_at_path(
            index,
            self.modules(),
            &path,
            shape.language,
            &import.target,
        );
        if !named.is_empty() {
            return Some(if named.contains(&c) {
                Verdict::Keep
            } else {
                Verdict::Import
            });
        }
        let segments: Vec<&str> = import.target.split('.').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return None;
        }
        let container = segments[segments.len() - 2];
        let imported = segments[segments.len() - 1];
        let cand = self.index.symbol(c);
        if cand.name != imported {
            return Some(Verdict::Import);
        }
        // `a.b.C.m`: a member of type/object `C`; `a.b.m` (lower-case qualifier): a top-level
        // function of package `a.b`.
        let member_of_type = container.starts_with(|ch: char| ch.is_uppercase());
        let ok = if member_of_type {
            cand.qualified_name == format!("{container}.{imported}")
                || cand.qualified_name.ends_with(&format!(".{container}.{imported}"))
        } else {
            cand.parent.is_none()
        };
        Some(if ok { Verdict::Keep } else { Verdict::Import })
    }

    /// Whether free function `c` (in another file) is reachable from the bare call.
    fn reachable(&mut self, shape: &CallShape<'i>, c: SymbolId) -> bool {
        let index: &'i Index = self.index;
        let cand = index.symbol(c);
        if cand.file == shape.file {
            return true;
        }
        match rules(shape.language).bare_reach {
            NameReach::Global => true,
            NameReach::Package => {
                relpath::parent(index.file_path(cand.file)) == relpath::parent(index.file_path(shape.file))
                    || self.import_files(shape.file, shape.owner, &shape.member).is_some()
            }
            NameReach::FileOrImports => {
                if let Some((files, _)) = self.import_files(shape.file, shape.owner, &shape.member) {
                    return files.contains(&cand.file);
                }
                // Wildcard imports: reachable when one resolves to the candidate's file;
                // an unresolvable wildcard import may bind anything.
                let imports = self.visible_imports(shape.file, shape.owner);
                let path = self.index.file_path(shape.file).to_string();
                let language = shape.language;
                for i in imports.iter().filter(|i| i.kind == ImportKind::Wildcard) {
                    let target = i.target.clone();
                    match self.modules().resolve(&path, language, &target, false) {
                        Some((files, _)) if files.contains(&cand.file) => return true,
                        Some(_) => {}
                        None => return true,
                    }
                }
                false
            }
        }
    }

    /// Whether `file` is an ES module (it imports or re-exports something): its top-level
    /// bindings are visible to other files only through an import.
    fn is_es_module(&self, file: FileId) -> bool {
        self.index
            .file(file)
            .facts
            .as_ref()
            .is_some_and(|f| !f.imports.is_empty() || !f.exports.is_empty())
    }

    fn verdict(&mut self, shape: &CallShape<'i>, pool: &[SymbolId], c: SymbolId) -> Verdict {
        let index = self.index;
        let language = shape.language;
        if !name_interop(language, index.file(index.symbol(c).file).language) {
            return Verdict::Scope;
        }
        let method = self.is_method(c);
        match &shape.form {
            CallForm::Bare {
                shadowed: true,
                local,
            } => {
                // A local / parameter binding of the callee name: never a class member (rule
                // 6; every language when the syntax facts prove the binding).
                if method && (locals_shadow(language) || *local) {
                    return Verdict::Scope;
                }
                // The name denotes the local binding (lexical shadowing): a declaration of
                // the same name is not visible here. Value flow may still deliver the
                // function the local holds.
                if *local || locals_shadow(language) {
                    return Verdict::Visibility;
                }
            }
            CallForm::Bare { shadowed: false, .. } => {
                // JVM languages: an explicit single-name import (`import okio.TestUtil.randomBytes`,
                // Java `import static a.B.m`) binds the simple name in the file. It decides the
                // candidate before the implicit-receiver rule (an imported object member is
                // reachable without an enclosing class).
                if let Some(v) = self.explicit_import_verdict(shape, c) {
                    return v;
                }
                if method {
                    match rules(language).bare_calls {
                        BareCalls::FunctionsOnly => {
                            // Python class-body code may call a method of that class.
                            let class_body = shape.call.and_then(|call| {
                                let d = call.lexical_owner?;
                                let s = index.file(shape.file).symbol_of_decl(d)?;
                                index.symbol(s).kind.is_type().then_some(s)
                            });
                            let own =
                                class_body.is_some_and(|k| self.hierarchy.class_of(index, c) == Some(k));
                            if !own {
                                return Verdict::Scope;
                            }
                        }
                        BareCalls::ImplicitReceiver => {
                            let class = self.hierarchy.class_of(index, c);
                            let (family, unknown) = self.implicit_receiver(shape);
                            if !unknown && !class.is_some_and(|k| family.contains(&k)) {
                                return Verdict::Scope;
                            }
                        }
                        BareCalls::Unknown => {}
                    }
                } else {
                    if let Some(p) = self.nesting_function(c) {
                        if nested_functions_are_lexical(language) && !self.encloses(p, shape.owner) {
                            return Verdict::Scope;
                        }
                    }
                    // Sourced scripts (Bash): a script that defines the function itself and
                    // sources no other file runs its own definition (the most recent
                    // definition wins).
                    let sourced = rules(language).modules == ModulePaths::SourcedFiles;
                    if sourced
                        && index.symbol(c).file != shape.file
                        && self.defines_own(shape.file, &shape.member)
                    {
                        return Verdict::Scope;
                    }
                    // A script sees only its own functions and those of the files it sources.
                    // When every `source` / `.` of the file resolves, a function in any other
                    // file is out (an unresolvable `source` may bring anything).
                    if sourced && index.symbol(c).file != shape.file {
                        if let Some(sourced) = self.sourced_files(shape.file) {
                            if !sourced.contains(&index.symbol(c).file) {
                                return Verdict::Import;
                            }
                        }
                    }
                    if self.nesting_function(c).is_none()
                        && index.symbol(c).parent.is_none()
                        && !self.reachable(shape, c)
                    {
                        // JavaScript / TypeScript modules: a top-level binding of another
                        // module is visible only through an import (non-exported bindings
                        // never are); script files (no import / export) share one scope.
                        let cand_file = index.symbol(c).file;
                        if rules(language).modules == ModulePaths::RelativeSpecifiers
                            && self.is_es_module(cand_file)
                            && self.is_es_module(shape.file)
                        {
                            return Verdict::Visibility;
                        }
                        return Verdict::Weak;
                    }
                }
            }
            CallForm::Receiver { .. } => {
                // Receiver is itself an allocation (`new K().m()`, Go `K{..}.m()`): an
                // exact type fact, applied to every blind call (a server that could not
                // type the file, e.g. a cgo file, leaves such calls blind too).
                if method {
                    if let Some(accept) = self.literal_receiver(shape, pool) {
                        let class = self.hierarchy.class_of(index, c);
                        if !class.is_some_and(|k| accept.contains(&k)) {
                            return Verdict::Receiver;
                        }
                    }
                }
            }
            CallForm::Path { .. } | CallForm::Other => {}
        }
        Verdict::Keep
    }

    /// Narrow `pool` (sorted by uid; the owner and anonymous scopes are skipped) for the
    /// call, collecting at most `limit` survivors (`truncated` when more exist).
    pub fn narrow(&mut self, shape: &CallShape<'i>, pool: &[SymbolId], limit: usize) -> Narrowing {
        let mut out = Narrowing::default();
        for &c in pool {
            if c == shape.owner || is_anonymous(self.index.symbol(c)) {
                continue;
            }
            let mut verdict = self.verdict(shape, pool, c);
            if matches!(verdict, Verdict::Keep | Verdict::Weak)
                && shape
                    .call
                    .is_some_and(|call| arity_rules_out(self.index, call, shape.detail, c))
            {
                verdict = Verdict::Arity;
            }
            match verdict {
                Verdict::Keep | Verdict::Weak if out.kept.len() >= limit => {
                    out.truncated = true;
                    break;
                }
                Verdict::Keep => out.kept.push(c),
                Verdict::Weak => {
                    out.kept.push(c);
                    out.weak.push(c);
                }
                Verdict::Import => out.dropped_by_import.push(c),
                Verdict::Receiver => out.dropped_by_receiver.push(c),
                Verdict::Scope => out.dropped_by_scope.push(c),
                Verdict::Visibility => out.dropped_by_visibility.push(c),
                Verdict::Arity => out.dropped_by_arity.push(c),
            }
        }
        out
    }
}

/// Number of arguments of a call whose arguments are all plain positional ones (the
/// [`trace_core::facts::arity_accepts`] contract): the call's structure is known
/// (`CallDetail`), agrees with the syntax argument count, and holds no keyword / named,
/// spread or unpacked argument. `None` otherwise.
pub(crate) fn positional_arguments(call: &CallSite, detail: Option<&CallDetail>) -> Option<u32> {
    let detail = detail?;
    if detail.arguments.len() as u32 != call.arg_count {
        return None;
    }
    detail
        .arguments
        .iter()
        .all(|a| matches!(a.slot, ArgSlot::Positional { exact: true, .. }))
        .then_some(call.arg_count)
}

/// Whether the callee names its member through a receiver value (`obj.m`, `obj->m`,
/// `obj?.m`), not a static path (`Type::m`, `A\m`) or a bare name.
fn member_call(call: &CallSite) -> bool {
    let Some(member) = call.member.as_deref().filter(|m| !m.is_empty()) else {
        return false;
    };
    matches!(split_callee(&call.callee, member).0, Some("recv"))
}

/// Rule `arity` (SPEC §7.11, §10.1): `candidate` can never be the target of the syntax call
/// `call` because its declared parameter list accepts no binding of the call's positional
/// arguments ([`trace_core::facts::arity_accepts`]). Positive evidence only:
/// * the call's arguments are all plain positional ones ([`positional_arguments`]);
/// * the candidate is a callable declaration of an analysed file and its own parameter list
///   is the one the call binds: not rewritten by a decorator / attribute macro (Python, Rust), not a
///   Rust foreign prototype (a C-style `...` is not recorded);
/// * the language defines the rule (`arity_accepts` decides).
///
/// A constructor binds the new instance like a receiver.
pub fn arity_rules_out(
    index: &Index,
    call: &CallSite,
    detail: Option<&CallDetail>,
    candidate: SymbolId,
) -> bool {
    let Some(args) = positional_arguments(call, detail) else {
        return false;
    };
    let s = index.symbol(candidate);
    if s.is_synthetic() || !s.kind.is_callable() {
        return false;
    }
    if rules(s.language).decorators_rewrite_signatures && !s.decorators.is_empty() {
        return false;
    }
    if rules(s.language).foreign_prototypes
        && s.is_stub
        && !s.parent.is_some_and(|p| index.symbol(p).kind.is_type())
    {
        return false;
    }
    let Some(decl) = index
        .file(s.file)
        .facts
        .as_ref()
        .and_then(|f| f.declarations.get(s.decl as usize))
        .filter(|d| d.name == s.name)
    else {
        return false;
    };
    let receiver_call = crate::hierarchy::is_constructor(s) || member_call(call);
    trace_core::facts::arity_accepts(s.language, &decl.parameters, args, receiver_call) == Some(false)
}

#[cfg(test)]
#[path = "../../tests/unit/narrow/mod.rs"]
mod tests;
