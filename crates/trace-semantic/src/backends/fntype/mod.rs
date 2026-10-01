//! The function-type rule (DESIGN §1.10 item 3; owner tables): a function passed to a
//! library parameter whose declared type is a function type -> "may run it" (inferred, never
//! proven). A top type (`object`, `any`, `interface{}`, `Object?`, an unconstrained type
//! parameter) accepts the function as a value and says nothing.
//!
//! Routes per language ([`FnTypeRoute`], chosen by the server hooks):
//! * **Label** (Go, PHP, C#, Scala): `signatureHelp` at the END
//!   of a named argument (inside an identifier gopls answers for the argument's own
//!   signature) or the START of an anonymous function argument ([`anonymous_arguments`]:
//!   closures, lambdas and arrow functions passed to a call are callback arguments too),
//!   every signature scanned (`activeSignature`/`activeParameter` are unreliable),
//!   the parameter chosen by the syntax index / keyword, its label parsed with the language's
//!   tree-sitter grammar inside a synthetic declaration. When the
//!   answer has no parameter for the argument (or the server does not support the request),
//!   the callee's `hover` declaration is parsed where the language has one (PHP:
//!   `hover_declaration`). In these
//!   languages a function value converts only to a function / delegate / SAM type or a top
//!   type, so a named non-top type is a function type (C# `Expression<..>` is data).
//! * **Declaration** (Python, Rust, C, C++; Haskell through `hover`):
//!   `definition` on the callee -> the library declaration file (read, never written) ->
//!   tree-sitter -> the parameter's declared type, every overload in the same scope; a named
//!   type is resolved with `definition` on the name inside the declaration file (aliases,
//!   typedefs, `TypeVar(bound=...)`, protocols with `__call__`, functor classes), at most
//!   [`MAX_HOPS`] hops; a C/C++ typedef declared earlier in the same file needs no hop, and a
//!   parameter behind an annotation macro (MSVC SAL `_In_ CompareFn _Compare`, read by the
//!   grammar with the macro as the type) is classified by the typedef its declarator names.
//! * **LanguageRule** (Java): a method reference / lambda argument always targets a
//!   functional interface; no request.
//! * **Checker** (JS/TS/TSX): inside the TypeScript worker (`assets/ts-worker/fntype.mjs`).
//! * **TableOnly** (Bash, R): no types; nothing here.
//!
//! Classifier data (named function types, top types, transparent wrappers, data types) come
//! from the language's table (`trace_library::table`). Answers are cached per declaration
//! file content hash + line + parameter selector ([`FnTypeCache`], bounded).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use trace_core::facts::{CallSite, CallbackArg, FileFacts};
use trace_core::semantics::{FnTypeVerdict, SemCallbackParam};
use trace_core::text::LineIndex;
use trace_core::Language;
use trace_library::table::Tables;
use tree_sitter::{Node, Tree};

use crate::SemanticError;

mod classes;
mod declaration;
mod hover;
mod label;

use self::classes::*;
use self::declaration::*;
use self::hover::*;
use self::label::*;

pub use self::label::anonymous_arguments;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnTypeRoute {
    Label,
    Declaration,
    LanguageRule,
    Checker,
    TableOnly,
}

impl FnTypeRoute {
    /// `SemCallbackParam::route` text.
    pub const fn as_str(self) -> &'static str {
        match self {
            FnTypeRoute::Label => "label",
            FnTypeRoute::Declaration => "declaration",
            FnTypeRoute::LanguageRule => "language_rule",
            FnTypeRoute::Checker => "checker",
            FnTypeRoute::TableOnly => "table_only",
        }
    }
}

/// One callback argument whose receiving call has no in-index target.
pub struct FnTypeQuery<'a> {
    pub language: Language,
    pub path: &'a str,
    pub source: &'a [u8],
    pub facts: &'a FileFacts,
    pub call: &'a CallSite,
    pub arg: &'a CallbackArg,
    pub route: FnTypeRoute,
}

/// The server requests the rule may make.
pub trait FnTypeSession {
    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, SemanticError>;
    /// Several requests as one pipelined batch, answers in request order (one [`request`]
    /// after another unless the session pipelines).
    ///
    /// [`request`]: FnTypeSession::request
    fn request_many(
        &mut self,
        calls: Vec<(String, serde_json::Value)>,
    ) -> Result<Vec<Result<serde_json::Value, SemanticError>>, SemanticError> {
        Ok(calls
            .into_iter()
            .map(|(method, params)| self.request(&method, params))
            .collect())
    }
    fn uri_of(&self, rel: &str) -> Result<String, SemanticError>;
    /// Read (never write) a declaration file the server pointed to: absolute path from a
    /// file URI.
    fn read_location(&mut self, uri: &str) -> Option<(PathBuf, Vec<u8>)>;
}

/// Named-type resolution depth (definition hops inside declaration files).
pub const MAX_HOPS: usize = 4;
/// Bound of the verdict cache.
const CACHE_LIMIT: usize = 50_000;
/// Parsed declaration files kept (most recent last).
const FILE_CACHE: usize = 8;
/// Recursion bound of type classification.
const MAX_DEPTH: u8 = 32;

/// Declaration file blake3 + line + parameter selector -> verdict (bounded to 50k entries),
/// plus the few declaration files parsed last.
#[derive(Default)]
pub struct FnTypeCache {
    verdicts: HashMap<(blake3::Hash, u32, String), Found>,
    docs: Vec<Arc<Doc>>,
}

impl FnTypeCache {
    fn verdict(&self, key: &(blake3::Hash, u32, String)) -> Option<Found> {
        self.verdicts.get(key).cloned()
    }

    fn remember(&mut self, key: (blake3::Hash, u32, String), found: Found) {
        if self.verdicts.len() >= CACHE_LIMIT {
            self.verdicts.clear();
        }
        self.verdicts.insert(key, found);
    }

    /// The parsed declaration file behind `uri` (read through the session, never written).
    fn doc(&mut self, session: &mut dyn FnTypeSession, uri: &str, hint: Language) -> Option<Arc<Doc>> {
        if let Some(i) = self.docs.iter().position(|d| d.uri == uri) {
            let doc = self.docs.remove(i);
            self.docs.push(Arc::clone(&doc));
            return Some(doc);
        }
        let (path, source) = session.read_location(uri)?;
        let language = declaration_language(hint, &path);
        let tree = trace_syntax::parse_tree(language, &source).ok()?;
        let doc = Arc::new(Doc {
            uri: uri.to_string(),
            path,
            language,
            hash: blake3::hash(&source),
            lines: LineIndex::new(&source),
            source,
            tree,
        });
        self.docs.push(Arc::clone(&doc));
        if self.docs.len() > FILE_CACHE {
            self.docs.remove(0);
        }
        Some(doc)
    }
}

/// A parsed declaration file.
struct Doc {
    uri: String,
    path: PathBuf,
    language: Language,
    hash: blake3::Hash,
    lines: LineIndex,
    source: Vec<u8>,
    tree: Tree,
}

/// The answer for one parameter.
#[derive(Clone, Debug, PartialEq)]
struct Found {
    verdict: FnTypeVerdict,
    param_type: String,
    param_name: Option<String>,
    symbol: Option<String>,
}

impl Found {
    fn unknown() -> Found {
        Found {
            verdict: FnTypeVerdict::Unknown,
            param_type: String::new(),
            param_name: None,
            symbol: None,
        }
    }
}

/// Which parameter an argument binds to (syntax index / keyword, never the server's
/// `activeParameter`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParamRef {
    index: Option<u32>,
    keyword: Option<String>,
}

impl ParamRef {
    fn of(arg: &CallbackArg) -> Option<ParamRef> {
        let keyword = arg.keyword.clone().filter(|k| !k.is_empty());
        (arg.index.is_some() || keyword.is_some()).then_some(ParamRef {
            index: arg.index,
            keyword,
        })
    }

    fn key(&self) -> String {
        format!("{:?}|{}", self.index, self.keyword.as_deref().unwrap_or(""))
    }
}

/// Classification of one type node.
#[derive(Clone, Debug, PartialEq)]
enum Class {
    Function,
    Top,
    Not,
    /// Holds a function as data, never runs it in-process (C# `Expression<..>`).
    Data,
    /// A named type still to resolve: the name and its byte offset in the parsed source
    /// (`usize::MAX` when the text is synthetic and cannot be followed).
    Named(String, usize),
    Unknown,
}

impl Class {
    fn rank(&self) -> u8 {
        match self {
            Class::Function => 5,
            Class::Named(..) => 4,
            Class::Top => 3,
            Class::Unknown => 2,
            Class::Not => 1,
            Class::Data => 0,
        }
    }

    fn verdict(&self) -> FnTypeVerdict {
        match self {
            Class::Function => FnTypeVerdict::FunctionType,
            Class::Top => FnTypeVerdict::TopType,
            Class::Not | Class::Data => FnTypeVerdict::NotFunctionType,
            Class::Named(..) | Class::Unknown => FnTypeVerdict::Unknown,
        }
    }
}

/// A union / overload set: a function type anywhere wins, then an unresolved name (to be
/// followed), then a top type; `Not` only when every member is a plain non-function type.
fn union(classes: impl IntoIterator<Item = Class>) -> Class {
    let mut best: Option<Class> = None;
    let mut all_data = true;
    for c in classes {
        all_data &= c == Class::Data;
        if best.as_ref().is_none_or(|b| c.rank() > b.rank()) {
            best = Some(c);
        }
    }
    match best {
        None => Class::Unknown,
        Some(Class::Data) if !all_data => Class::Not,
        Some(c) => c,
    }
}

/// Verdict order across overloads: FunctionType > TopType > NotFunctionType > Unknown.
fn verdict_rank(v: FnTypeVerdict) -> u8 {
    match v {
        FnTypeVerdict::FunctionType => 3,
        FnTypeVerdict::TopType => 2,
        FnTypeVerdict::NotFunctionType => 1,
        FnTypeVerdict::Unknown => 0,
    }
}

fn better(best: Option<Found>, found: Found) -> Option<Found> {
    match best {
        Some(b) if verdict_rank(b.verdict) >= verdict_rank(found.verdict) => Some(b),
        _ => Some(found),
    }
}

/// The embedded classifier data.
fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(Tables::builtin)
}

/// Grammar for a declaration file: C/C++ headers (often without extension) use the caller's
/// grammar, everything else its extension.
fn declaration_language(hint: Language, path: &std::path::Path) -> Language {
    match hint {
        Language::C | Language::Cpp => hint,
        _ => trace_core::languages::from_path(path).unwrap_or(hint),
    }
}

/// None = no request made (route TableOnly/Checker, or not applicable). Bounded: <= 4
/// definition hops.
pub fn resolve(
    q: &FnTypeQuery<'_>,
    session: &mut dyn FnTypeSession,
    cache: &mut FnTypeCache,
) -> Option<SemCallbackParam> {
    match q.route {
        FnTypeRoute::TableOnly | FnTypeRoute::Checker => None,
        FnTypeRoute::LanguageRule => language_rule(q),
        FnTypeRoute::Label => label_route(q, session),
        FnTypeRoute::Declaration => match q.language {
            Language::Haskell => hover_route(q, session),
            _ => declaration_route(q, session, cache),
        },
    }
}

/// Apply the language's policy to an unresolved name: where a function value converts only
/// to a function-like or a top type, a named non-top type is a function type.
fn settle(language: Language, class: Class) -> Class {
    match class {
        Class::Named(..) if assignability_decides(language) => Class::Function,
        Class::Named(..) if language == Language::Php => Class::Not,
        other => other,
    }
}

fn assignability_decides(language: Language) -> bool {
    matches!(language, Language::Go | Language::CSharp | Language::Scala | Language::Java)
}

/// The answer record.
fn answer(q: &FnTypeQuery<'_>, route: FnTypeRoute, found: Option<Found>) -> SemCallbackParam {
    let f = found.unwrap_or_else(Found::unknown);
    SemCallbackParam {
        call: q.call.callee_span,
        arg: q.arg.arg_span,
        param_name: f.param_name,
        param_type: f.param_type,
        verdict: f.verdict,
        route: route.as_str().to_string(),
        library_symbol: f.symbol,
    }
}

// ---------------------------------------------------------------------------------------
// Tree helpers

fn text<'s>(n: Node<'_>, src: &'s [u8]) -> &'s str {
    n.utf8_text(src).unwrap_or("")
}

fn named_kids<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = n.walk();
    n.named_children(&mut cursor).collect()
}

fn kids<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = n.walk();
    n.children(&mut cursor).collect()
}

fn first_named(n: Node<'_>) -> Option<Node<'_>> {
    n.named_child(0)
}

/// Pre-order search for the first node of one of `kinds` inside `[start, end)`.
fn find_kind<'t>(n: Node<'t>, kinds: &[&str], start: usize, end: usize) -> Option<Node<'t>> {
    if n.end_byte() <= start || n.start_byte() >= end {
        return None;
    }
    if kinds.contains(&n.kind()) && n.start_byte() >= start && n.end_byte() <= end {
        return Some(n);
    }
    kids(n).into_iter().find_map(|c| find_kind(c, kinds, start, end))
}

/// Every node of `kinds` in the tree (pre-order), bounded.
fn all_of_kind<'t>(root: Node<'t>, kinds: &[&str], limit: usize) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if out.len() >= limit {
            break;
        }
        if kinds.contains(&n.kind()) {
            out.push(n);
        }
        let mut children = kids(n);
        children.reverse();
        stack.extend(children);
    }
    out
}

/// The largest node spanning exactly `[start, end)`.
fn exact_node(root: Node<'_>, start: usize, end: usize) -> Option<Node<'_>> {
    let mut n = root.descendant_for_byte_range(start, end)?;
    while let Some(p) = n.parent() {
        if p.start_byte() == start && p.end_byte() == end {
            n = p;
        } else {
            break;
        }
    }
    (n.start_byte() == start && n.end_byte() == end).then_some(n)
}

/// Last segment of a qualified type name (`typing.Callable`, `std::function`, `\Closure`).
fn last_segment(name: &str) -> &str {
    name.rsplit(['.', ':', '\\']).next().unwrap_or(name)
}

/// A type name without generic arguments (`std::function<void()>` -> `std::function`).
fn base_name(name: &str) -> &str {
    name.split(['<', '[']).next().unwrap_or(name).trim()
}

fn same_type_name(listed: &str, name: &str) -> bool {
    let name = name.trim().trim_start_matches('\\');
    listed == name || last_segment(listed) == last_segment(name)
}

/// A named type from the language's classifier data.
fn name_class(language: Language, name: &str, tables: &Tables) -> Option<Class> {
    let name = base_name(name);
    if name.is_empty() {
        return None;
    }
    if tables.data_types(language).iter().any(|t| same_type_name(t, name)) {
        return Some(Class::Data);
    }
    if tables.top_types(language).iter().any(|t| same_type_name(t, name)) {
        return Some(Class::Top);
    }
    if tables
        .function_types(language)
        .iter()
        .any(|t| same_type_name(t, name))
    {
        return Some(Class::Function);
    }
    None
}

fn is_wrapper(language: Language, name: &str, tables: &Tables) -> bool {
    let name = base_name(name);
    tables.wrapper_types(language).iter().any(|t| same_type_name(t, name))
}

/// Classification context: the parsed source and the enclosing declaration's type
/// parameters (name -> class of their bounds).
struct Cx<'a> {
    language: Language,
    src: &'a [u8],
    tables: &'a Tables,
    type_params: HashMap<String, Class>,
}

impl Cx<'_> {
    fn named(&self, name: &str, at: usize) -> Class {
        let name = name.trim();
        if let Some(c) = self.type_params.get(base_name(name)) {
            return c.clone();
        }
        if let Some(c) = name_class(self.language, name, self.tables) {
            return c;
        }
        Class::Named(base_name(name).to_string(), at)
    }

    fn text(&self, n: Node<'_>) -> &str {
        text(n, self.src)
    }
}

// ---------------------------------------------------------------------------------------
// LanguageRule (Java)

/// Java: a method reference or lambda argument always targets a functional interface.
fn language_rule(q: &FnTypeQuery<'_>) -> Option<SemCallbackParam> {
    if q.language != Language::Java {
        return None;
    }
    let argument = q.arg.argument.trim();
    if argument.is_empty() {
        return None;
    }
    const PRE: &str = "class _T { Object _f = ";
    let wrapped = format!("{PRE}{argument}; }}");
    let tree = trace_syntax::parse_tree(Language::Java, wrapped.as_bytes()).ok()?;
    let node = exact_node(tree.root_node(), PRE.len(), PRE.len() + argument.len())?;
    if !matches!(node.kind(), "method_reference" | "lambda_expression") {
        return None;
    }
    Some(answer(
        q,
        FnTypeRoute::LanguageRule,
        Some(Found {
            verdict: FnTypeVerdict::FunctionType,
            param_type: String::new(),
            param_name: None,
            symbol: None,
        }),
    ))
}

#[cfg(test)]
#[path = "../../../tests/unit/backends/fntype/mod.rs"]
mod tests;
