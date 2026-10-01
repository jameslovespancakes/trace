//! Tiny native tables (DESIGN §1.12; owner tables): `assets/library/<lang>.json`, embedded.
//!
//! Entries exist only for behaviour with neither readable source nor a useful declared type;
//! every entry says `why` and names its [`Basis`]. The same files carry the function-type
//! classifier data (`function_types`, `top_types`, `wrapper_types`, `data_types`) and the
//! irreducible sections of PLAN decision 14 ([`Section`], every row with
//! `why_not_derivable`), including the `syntax_conventions` rows that trace-syntax reads for
//! binding attributes, generated RPC names and GraphQL resolver conventions (the only place
//! such package-specific names exist in trace).
//!
//! Lookups: [`Tables::by_symbol`] (the library-qualified symbol the language server resolved,
//! incl. the entry's `aliases`) and [`Tables::by_spelling`] (only for callees that resolve to
//! nothing: bare names and last-segment qualifiers, never methods, which would be a
//! method-name union).
//!
//! Symbol conventions (shared with derive's `symbol.rs`): `<module>.<qualified name>` with the
//! language's own separator (`builtins.map`, `std::sort`, `Closure::fromCallable`,
//! `encoding/json.Marshal` for Go import paths);
//! constructors are keyed by the type (`threading.Thread`, `System.Lazy`); R leaves are keyed
//! by their native spelling (`.Internal(lapply)`, `.Primitive("forceAndCall")`,
//! `.External2(C_optim)`); Bash commands by the command word.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trace_core::Language;

use crate::languages;
use crate::model::{ArgSel, Channel, Effect, LibraryError, VerbSel};

/// Current table schema.
const TABLE_SCHEMA: u32 = 1;

/// Irreducible sections (PLAN decision 14, DESIGN-bridges §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    IoSend,
    IoEntry,
    ReflectionRoots,
    FsRoutes,
    RoutePatterns,
    FfiConventions,
    TestConventions,
    RuntimeDispatch,
    /// Conventions read by the syntax facts of `trace_syntax::boundary` (binding attributes
    /// of procedural macros, names of generated RPC code, GraphQL resolver conventions,
    /// addon registration calls): every row names one generic rule of [`SYNTAX_RULES`] and
    /// the bridge kind of the facts it gives. The rows are embedded into trace-syntax at
    /// build time: changing them changes extracted facts (bump `EXTRACTOR_VERSION`).
    SyntaxConventions,
}

impl Section {
    pub const ALL: [Section; 9] = [
        Section::IoSend,
        Section::IoEntry,
        Section::ReflectionRoots,
        Section::FsRoutes,
        Section::RoutePatterns,
        Section::FfiConventions,
        Section::TestConventions,
        Section::RuntimeDispatch,
        Section::SyntaxConventions,
    ];

    /// JSON key of the section.
    pub const fn key(self) -> &'static str {
        match self {
            Section::IoSend => "io_send",
            Section::IoEntry => "io_entry",
            Section::ReflectionRoots => "reflection_roots",
            Section::FsRoutes => "fs_routes",
            Section::RoutePatterns => "route_patterns",
            Section::FfiConventions => "ffi_conventions",
            Section::TestConventions => "test_conventions",
            Section::RuntimeDispatch => "runtime_dispatch",
            Section::SyntaxConventions => "syntax_conventions",
        }
    }
}

/// Allowed `why_not_derivable` values of irreducible rows. `generated_code`: the behaviour
/// is code a generator or a procedural macro writes at build time (binding glue, RPC stubs,
/// macro expansions the language server does not deliver); it is not on disk to derive from.
pub const WHY_NOT_DERIVABLE: [&str; 6] = [
    "compiled_runtime",
    "reflection",
    "filesystem_convention",
    "language_spec",
    "no_source",
    "generated_code",
];

/// What a `syntax_conventions` row must hold besides `rule`, `bridge` and the reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxRuleShape {
    /// A `symbol` (attribute, decorator, annotation, function, member or key name).
    Symbol,
    /// A `pattern` with exactly one `<Service>` placeholder (generated RPC naming).
    ServicePattern,
    /// A `symbol` (the resolver base type) and a `pattern` with exactly one `<field>`
    /// placeholder (the resolver method name).
    SymbolAndFieldPattern,
    /// A `symbol` (the registration function or macro) with `key` (argument naming the
    /// export) and `handler` (argument holding the function) selectors.
    SymbolKeyAndHandler,
}

/// The generic rules of `trace_syntax::boundary` that `syntax_conventions` rows feed (the
/// rows hold every package-specific name; the rule is the structure trace reads):
/// * binding attributes on Rust items (`export_*_attribute`, `export_enum_variants`,
///   `rename_attribute`, `name_key`, `module_key`, `constructor_attribute` (`value`: the
///   member name the constructor gets, e.g. `__new__`), `constructor_flag`,
///   `camel_case_names`, `register_macro`, `register_member`, `import_block_attribute`);
/// * addon registration in C / C++ (`registration_call`, `addon_export_member`,
///   `addon_function_constructor`) and addon loading in JavaScript (`addon_loader`);
/// * names of generated RPC code (`rpc_client_type`, `rpc_client_constructor`,
///   `rpc_stub_class`, `rpc_stub_factory`, `rpc_credentials`, `rpc_server_base`,
///   `rpc_server_register`, `rpc_server_embed`, `rpc_add_service`);
/// * GraphQL resolver conventions (`graphql_document_tag`, `graphql_resolver_map`,
///   `graphql_type_decorator`, `graphql_field_decorator`, `graphql_resolver_base`,
///   `graphql_root_type_object` (`value`: the root type), `graphql_type_object`,
///   `graphql_field_member`, `graphql_field_annotation` (`value`: the root type, or `key`:
///   the element naming it), `graphql_field_name_element`).
const SYNTAX_RULES: [(&str, SyntaxRuleShape); 39] = [
    ("export_function_attribute", SyntaxRuleShape::Symbol),
    ("export_type_attribute", SyntaxRuleShape::Symbol),
    ("export_members_attribute", SyntaxRuleShape::Symbol),
    ("export_public_members_attribute", SyntaxRuleShape::Symbol),
    ("export_marked_members_attribute", SyntaxRuleShape::Symbol),
    ("export_module_attribute", SyntaxRuleShape::Symbol),
    ("export_enum_variants", SyntaxRuleShape::Symbol),
    ("rename_attribute", SyntaxRuleShape::Symbol),
    ("name_key", SyntaxRuleShape::Symbol),
    ("module_key", SyntaxRuleShape::Symbol),
    ("constructor_attribute", SyntaxRuleShape::Symbol),
    ("constructor_flag", SyntaxRuleShape::Symbol),
    ("camel_case_names", SyntaxRuleShape::Symbol),
    ("register_macro", SyntaxRuleShape::Symbol),
    ("register_member", SyntaxRuleShape::Symbol),
    ("import_block_attribute", SyntaxRuleShape::Symbol),
    ("registration_call", SyntaxRuleShape::SymbolKeyAndHandler),
    ("addon_export_member", SyntaxRuleShape::Symbol),
    ("addon_function_constructor", SyntaxRuleShape::Symbol),
    ("addon_loader", SyntaxRuleShape::Symbol),
    ("rpc_client_type", SyntaxRuleShape::ServicePattern),
    ("rpc_client_constructor", SyntaxRuleShape::ServicePattern),
    ("rpc_stub_class", SyntaxRuleShape::ServicePattern),
    ("rpc_stub_factory", SyntaxRuleShape::Symbol),
    ("rpc_credentials", SyntaxRuleShape::Symbol),
    ("rpc_server_base", SyntaxRuleShape::ServicePattern),
    ("rpc_server_register", SyntaxRuleShape::ServicePattern),
    ("rpc_server_embed", SyntaxRuleShape::ServicePattern),
    ("rpc_add_service", SyntaxRuleShape::Symbol),
    ("graphql_document_tag", SyntaxRuleShape::Symbol),
    ("graphql_resolver_map", SyntaxRuleShape::Symbol),
    ("graphql_type_decorator", SyntaxRuleShape::Symbol),
    ("graphql_field_decorator", SyntaxRuleShape::Symbol),
    ("graphql_resolver_base", SyntaxRuleShape::SymbolAndFieldPattern),
    ("graphql_root_type_object", SyntaxRuleShape::Symbol),
    ("graphql_type_object", SyntaxRuleShape::Symbol),
    ("graphql_field_member", SyntaxRuleShape::Symbol),
    ("graphql_field_annotation", SyntaxRuleShape::Symbol),
    ("graphql_field_name_element", SyntaxRuleShape::Symbol),
];

/// The shape of a `syntax_conventions` rule (None: not a rule trace implements).
pub fn syntax_rule_shape(rule: &str) -> Option<SyntaxRuleShape> {
    SYNTAX_RULES
        .iter()
        .find(|(name, _)| *name == rule)
        .map(|(_, shape)| *shape)
}

/// Allowed entry kinds.
const ENTRY_KINDS: [&str; 6] = ["function", "method", "constructor", "command", "macro", "protocol"];

/// Entry kinds that the spelling fallback may match (a method needs its receiver's type,
/// a protocol row has no call spelling).
const SPELLABLE_KINDS: [&str; 4] = ["function", "constructor", "command", "macro"];

/// Why a native entry exists: the class of reason (`why` says it in words).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// Compiled built-in: the runtime ships no readable source for it (checked by
    /// `rule_table_row_has_no_readable_source` where the toolchain is installed).
    NoSource,
    /// Readable source exists, but the call happens through runtime dispatch the generic
    /// derivation rules cannot follow (native thread entry, interface upgrade by type
    /// assertion, a function pickled to a worker process, a finalizer run by the GC).
    RuntimeDispatch,
    /// An implicit call defined by the language specification (protocol methods).
    LanguageSpec,
    /// A function-typed parameter that is never called (overrides the function-type rule).
    NeverCalls,
    /// The declared type only says "may run it"; the row says when (now, later, on access).
    Timing,
    /// The parameter has no useful declared type (unconstrained template parameter) and the implementation behind it is not followed by the generic rules
    /// (library-internal helpers, compiler intrinsics).
    Untyped,
}

impl Basis {
    pub const ALL: [Basis; 6] = [
        Basis::NoSource,
        Basis::RuntimeDispatch,
        Basis::LanguageSpec,
        Basis::NeverCalls,
        Basis::Timing,
        Basis::Untyped,
    ];

    /// JSON value of the basis.
    pub const fn key(self) -> &'static str {
        match self {
            Basis::NoSource => "no_source",
            Basis::RuntimeDispatch => "runtime_dispatch",
            Basis::LanguageSpec => "language_spec",
            Basis::NeverCalls => "never_calls",
            Basis::Timing => "timing",
            Basis::Untyped => "untyped",
        }
    }
}

/// One native-table entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableEntry {
    /// Library-qualified symbol (declaring type for methods, the type for constructors).
    pub symbol: String,
    /// Other library-qualified symbols with exactly the same behaviour.
    pub aliases: Vec<String>,
    /// function | method | constructor | command | macro | protocol
    pub kind: String,
    /// Exact positional count, when the entry applies only to it.
    pub arity: Option<u32>,
    pub effects: Vec<Effect>,
    pub describe: String,
    /// Why this cannot be derived (required).
    pub why: String,
    pub basis: Basis,
}

impl TableEntry {
    /// The symbol followed by its aliases.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.symbol.as_str()).chain(self.aliases.iter().map(String::as_str))
    }

    /// Whether `symbol` is this entry's symbol or one of its aliases.
    pub fn answers(&self, symbol: &str) -> bool {
        self.names().any(|n| n == symbol)
    }

    /// Whether a call with `positional` positional arguments matches the entry's arity.
    pub(crate) fn accepts_arity(&self, positional: u32) -> bool {
        self.arity.is_none_or(|a| a == positional)
    }

    /// A method entry (receiver typed by the language server; never matched by spelling).
    pub fn is_method(&self) -> bool {
        self.kind == "method"
    }

    /// Whether the entry denies a call (`never_calls` rows override the function-type rule).
    pub fn never_calls(&self) -> bool {
        self.effects.iter().any(|e| matches!(e, Effect::NeverCalls(_)))
    }
}

/// One irreducible row (selectors kept as JSON; typed through [`IrreducibleRow::key_sel`],
/// [`IrreducibleRow::handler_sel`], [`IrreducibleRow::verb_sel`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IrreducibleRow {
    pub symbol: Option<String>,
    pub pattern: Option<String>,
    pub glob: Option<String>,
    /// `fs_routes`: the directory (relative to the glob's base) that is the URL root, e.g.
    /// `pages` for `pages/api/**` (`/api/...`), `server` for `server/api/**`.
    pub root: Option<String>,
    pub channel: Option<Channel>,
    pub key: Option<Value>,
    pub verb: Option<Value>,
    pub handler: Option<Value>,
    /// Row active only when this package is an installed dependency (checked dynamically
    /// through trace-env); required for `fs_routes`.
    pub activated_by: Option<String>,
    /// `syntax_conventions`: the generic rule of `trace_syntax::boundary` the row feeds (one
    /// of [`SYNTAX_RULES`]).
    pub rule: Option<String>,
    /// `syntax_conventions`: the bridge kind of the facts the rule gives
    /// (`trace_core::model::BridgeKind` name: `pyo3`, `grpc`, ...).
    pub bridge: Option<String>,
    /// `syntax_conventions`: the rule's value (the GraphQL root type an annotation or a type
    /// object stands for; the member name a constructor gets).
    pub value: Option<String>,
    pub why_not_derivable: String,
    pub describe: String,
}

/// Where an irreducible row finds its key or handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowSel {
    /// An argument of the call (same selectors as native entries).
    Arg(ArgSel),
    /// The route key is the file path relative to the glob's root (`fs_routes`).
    FilePath,
    /// The handler is the module's default export (`fs_routes`).
    DefaultExport,
    /// Every exported function whose name is an HTTP verb is a handler (`fs_routes`).
    ExportedNames,
}

/// The verb of an irreducible row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowVerb {
    Sel(VerbSel),
    /// The verb is the exported function's name (`export function GET`).
    ExportedNames,
}

impl IrreducibleRow {
    /// The row's anchor text: pattern, else symbol, else glob.
    pub fn anchor(&self) -> &str {
        self.pattern
            .as_deref()
            .or(self.symbol.as_deref())
            .or(self.glob.as_deref())
            .unwrap_or("")
    }

    /// Typed `key` (None when the row has none).
    pub fn key_sel(&self) -> Result<Option<RowSel>, String> {
        self.key.as_ref().map(parse_row_selector).transpose()
    }

    /// Typed `handler` (None when the row has none).
    pub fn handler_sel(&self) -> Result<Option<RowSel>, String> {
        self.handler.as_ref().map(parse_row_selector).transpose()
    }

    /// Typed `verb` (None when the row has none).
    pub fn verb_sel(&self) -> Result<Option<RowVerb>, String> {
        self.verb.as_ref().map(parse_verb).transpose()
    }

    /// Whether the row applies: no `activated_by`, or that package is installed.
    pub fn active(&self, installed: &dyn Fn(&str) -> bool) -> bool {
        self.activated_by.as_deref().is_none_or(installed)
    }
}

/// One language's table file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LanguageTable {
    pub language: String,
    pub runtime: String,
    /// Namespaces of the language runtime / standard library; every entry symbol lies under
    /// one of them (no third-party rows).
    pub roots: Vec<String>,
    /// Namespaces whose members are visible by bare name (`builtins`, `Kernel`).
    pub prelude: Vec<String>,
    /// Named function types (classifier data of the function-type rule).
    pub function_types: Vec<String>,
    /// Named top types: never a function type.
    pub top_types: Vec<String>,
    /// Transparent wrappers: the type argument decides (`Optional[..]`, `Box<..>`).
    pub wrapper_types: Vec<String>,
    /// Types that hold a function as data and never run it in-process
    /// (`System.Linq.Expressions.Expression`).
    pub data_types: Vec<String>,
    pub entries: Vec<TableEntry>,
    pub irreducible: BTreeMap<Section, Vec<IrreducibleRow>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTable {
    schema: u32,
    language: String,
    #[serde(default)]
    runtime: String,
    /// Free text: selector conventions of this file (documentation for people, not read).
    #[serde(default, rename = "conventions")]
    _conventions: String,
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    prelude: Vec<String>,
    #[serde(default)]
    function_types: Vec<String>,
    #[serde(default)]
    top_types: Vec<String>,
    #[serde(default)]
    wrapper_types: Vec<String>,
    #[serde(default)]
    data_types: Vec<String>,
    #[serde(default)]
    entries: Vec<RawEntry>,
    #[serde(default)]
    io_send: Vec<IrreducibleRow>,
    #[serde(default)]
    io_entry: Vec<IrreducibleRow>,
    #[serde(default)]
    reflection_roots: Vec<IrreducibleRow>,
    #[serde(default)]
    fs_routes: Vec<IrreducibleRow>,
    #[serde(default)]
    route_patterns: Vec<IrreducibleRow>,
    #[serde(default)]
    ffi_conventions: Vec<IrreducibleRow>,
    #[serde(default)]
    test_conventions: Vec<IrreducibleRow>,
    #[serde(default)]
    runtime_dispatch: Vec<IrreducibleRow>,
    #[serde(default)]
    syntax_conventions: Vec<IrreducibleRow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    symbol: String,
    #[serde(default)]
    aliases: Vec<String>,
    kind: String,
    #[serde(default)]
    arity: Option<u32>,
    effects: Vec<Value>,
    #[serde(default)]
    describe: String,
    #[serde(default)]
    why: String,
    basis: Basis,
}

/// Every language's table.
#[derive(Clone, Debug, Default)]
pub struct Tables {
    by_file: BTreeMap<String, LanguageTable>,
}

impl Tables {
    /// The embedded tables. The embedded files are validated by `tests/rule_tables.rs` (an
    /// invalid embedded file fails the test suite, it never ships); `Library::open` uses
    /// [`Tables::load_builtin`] and reports the error.
    pub fn builtin() -> Tables {
        Tables::load_builtin().unwrap_or_default()
    }

    /// Parse and validate the embedded tables.
    pub fn load_builtin() -> Result<Tables, LibraryError> {
        let mut by_file = BTreeMap::new();
        for spec in languages::ALL {
            let key = spec.languages[0].as_str();
            by_file.insert(key.to_string(), parse_table(key, spec.table)?);
        }
        Ok(Tables { by_file })
    }

    /// The table of `language` (None for languages without a table file).
    pub fn table(&self, language: Language) -> Option<&LanguageTable> {
        self.by_file.get(languages::table_language(language).as_str())
    }

    /// Every native entry of `language`.
    pub fn entries(&self, language: Language) -> &[TableEntry] {
        self.table(language).map(|t| t.entries.as_slice()).unwrap_or(&[])
    }

    /// Entries whose symbol or alias is `symbol` (callers check
    /// [`TableEntry::accepts_arity`]).
    pub fn by_symbol(&self, language: Language, symbol: &str) -> Vec<&TableEntry> {
        self.entries(language).iter().filter(|e| e.answers(symbol)).collect()
    }

    /// Spelling fallback only for callees that resolve to nothing in the index or a library
    /// (Bash builtins, R primitives): a bare name matches a symbol
    /// without namespace or in the language's prelude; a qualifier matches the symbol's
    /// namespace exactly or by its last segment. Methods and protocol rows never match (that
    /// would be a method-name union); the arity must match.
    pub fn by_spelling(
        &self,
        language: Language,
        qualifier: Option<&str>,
        name: &str,
        positional: u32,
    ) -> Vec<&TableEntry> {
        let Some(table) = self.table(language) else {
            return Vec::new();
        };
        let qualifier = qualifier.map(str::trim).filter(|q| !q.is_empty());
        table
            .entries
            .iter()
            .filter(|e| SPELLABLE_KINDS.contains(&e.kind.as_str()) && e.accepts_arity(positional))
            .filter(|e| {
                if e.kind == "command" && qualifier.is_some() {
                    return false;
                }
                e.names().any(|s| spelled(s, qualifier, name, &table.prelude))
            })
            .collect()
    }

    pub fn function_types(&self, language: Language) -> &[String] {
        self.table(language)
            .map(|t| t.function_types.as_slice())
            .unwrap_or(&[])
    }

    pub fn top_types(&self, language: Language) -> &[String] {
        self.table(language).map(|t| t.top_types.as_slice()).unwrap_or(&[])
    }

    pub fn wrapper_types(&self, language: Language) -> &[String] {
        self.table(language)
            .map(|t| t.wrapper_types.as_slice())
            .unwrap_or(&[])
    }

    pub fn data_types(&self, language: Language) -> &[String] {
        self.table(language).map(|t| t.data_types.as_slice()).unwrap_or(&[])
    }

    pub fn irreducible(&self, language: Language, section: Section) -> &[IrreducibleRow] {
        self.table(language)
            .and_then(|t| t.irreducible.get(&section))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Split a symbol into (namespace, last name) at its last separator (`::`, `#`, `.`); a
/// symbol with a parenthesised native spelling (`.Internal(lapply)`) is one name.
pub fn split_symbol(symbol: &str) -> (&str, &str) {
    if symbol.contains('(') {
        return ("", symbol);
    }
    let candidates = [
        symbol.rfind("::").map(|i| (i, 2)),
        symbol.rfind('#').map(|i| (i, 1)),
        symbol.rfind('.').map(|i| (i, 1)),
    ];
    match candidates.into_iter().flatten().max_by_key(|(i, _)| *i) {
        // A leading separator (`:erlang` has none; `.x` would) keeps the whole symbol.
        Some((0, _)) | None => ("", symbol),
        Some((i, len)) => (&symbol[..i], &symbol[i + len..]),
    }
}

/// Last segment of a namespace or qualifier (`encoding/json` -> `json`, `self.table` -> `table`).
fn last_segment(text: &str) -> &str {
    let (_, last) = split_symbol(text);
    last.rsplit('/').next().unwrap_or(last)
}

/// Whether the spelling (qualifier, name) names the table symbol `symbol`.
fn spelled(symbol: &str, qualifier: Option<&str>, name: &str, prelude: &[String]) -> bool {
    let (namespace, last) = split_symbol(symbol);
    if last != name {
        return false;
    }
    match qualifier {
        None => namespace.is_empty() || prelude.iter().any(|p| p == namespace),
        Some(q) => !namespace.is_empty() && (namespace == q || last_segment(namespace) == last_segment(q)),
    }
}

/// Whether `symbol` lies under one of `roots` (equal, or followed by a separator).
fn under_root(symbol: &str, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        symbol == root
            || symbol.strip_prefix(root.as_str()).is_some_and(|rest| {
                rest.starts_with('.')
                    || rest.starts_with(':')
                    || rest.starts_with('#')
                    || rest.starts_with('(')
                    || rest.starts_with('/')
            })
    })
}

/// Parse and validate one table file.
pub fn parse_table(key: &str, text: &str) -> Result<LanguageTable, LibraryError> {
    let err = |message: String| LibraryError::Table {
        language: key.to_string(),
        message,
    };
    let raw: RawTable = serde_json::from_str(text).map_err(|e| err(e.to_string()))?;
    if raw.schema != TABLE_SCHEMA {
        return Err(err(format!("schema {} (expected {TABLE_SCHEMA})", raw.schema)));
    }
    if raw.language != key {
        return Err(err(format!("file says language {:?}", raw.language)));
    }
    if raw.runtime.trim().is_empty() {
        return Err(err("`runtime` must name the runtime the rows describe".into()));
    }
    if raw.function_types.is_empty() || raw.top_types.is_empty() {
        return Err(err(
            "`function_types` and `top_types` are required (classifier data of the function-type rule)"
                .into(),
        ));
    }
    if !raw.entries.is_empty() && raw.roots.is_empty() {
        return Err(err("`roots` is required when the file has entries".into()));
    }
    let mut seen: BTreeSet<(String, Option<u32>)> = BTreeSet::new();
    let mut entries = Vec::with_capacity(raw.entries.len());
    for e in raw.entries {
        let at = |m: &str| err(format!("{}: {m}", e.symbol));
        if e.symbol.trim().is_empty() {
            return Err(err("an entry has an empty symbol".into()));
        }
        if e.why.trim().is_empty() {
            return Err(at("every entry needs `why`"));
        }
        if e.describe.trim().is_empty() {
            return Err(at("every entry needs `describe`"));
        }
        if !ENTRY_KINDS.contains(&e.kind.as_str()) {
            return Err(at(&format!("unknown kind {:?}", e.kind)));
        }
        if e.effects.is_empty() {
            return Err(at("every entry needs at least one effect"));
        }
        let effects = e
            .effects
            .iter()
            .map(parse_effect)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|m| at(&m))?;
        if e.basis == Basis::NeverCalls && !effects.iter().all(|x| matches!(x, Effect::NeverCalls(_))) {
            return Err(at("a `never_calls` entry may only hold never_calls effects"));
        }
        for name in std::iter::once(&e.symbol).chain(e.aliases.iter()) {
            if name.trim().is_empty() {
                return Err(at("empty alias"));
            }
            if !under_root(name, &raw.roots) {
                return Err(at(&format!(
                    "{name:?} is not under the runtime roots {:?} (no third-party rows)",
                    raw.roots
                )));
            }
            if !seen.insert((name.clone(), e.arity)) {
                return Err(at(&format!("duplicate row for {name:?}")));
            }
        }
        entries.push(TableEntry {
            symbol: e.symbol,
            aliases: e.aliases,
            kind: e.kind,
            arity: e.arity,
            effects,
            describe: e.describe,
            why: e.why,
            basis: e.basis,
        });
    }
    let mut irreducible = BTreeMap::new();
    let mut seen_rows: BTreeSet<String> = BTreeSet::new();
    for (section, rows) in [
        (Section::IoSend, raw.io_send),
        (Section::IoEntry, raw.io_entry),
        (Section::ReflectionRoots, raw.reflection_roots),
        (Section::FsRoutes, raw.fs_routes),
        (Section::RoutePatterns, raw.route_patterns),
        (Section::FfiConventions, raw.ffi_conventions),
        (Section::TestConventions, raw.test_conventions),
        (Section::RuntimeDispatch, raw.runtime_dispatch),
        (Section::SyntaxConventions, raw.syntax_conventions),
    ] {
        for row in &rows {
            validate_row(section, row)
                .map_err(|m| err(format!("{} {:?}: {m}", section.key(), row.anchor())))?;
            let identity = format!(
                "{}|{:?}|{:?}|{:?}|{:?}|{}|{:?}|{:?}",
                section.key(),
                row.symbol,
                row.pattern,
                row.glob,
                row.activated_by,
                row.key.as_ref().map(Value::to_string).unwrap_or_default(),
                row.rule,
                row.bridge
            );
            if !seen_rows.insert(identity) {
                return Err(err(format!("{}: duplicate row {:?}", section.key(), row.anchor())));
            }
        }
        if !rows.is_empty() {
            irreducible.insert(section, rows);
        }
    }
    Ok(LanguageTable {
        language: raw.language,
        runtime: raw.runtime,
        roots: raw.roots,
        prelude: raw.prelude,
        function_types: raw.function_types,
        top_types: raw.top_types,
        wrapper_types: raw.wrapper_types,
        data_types: raw.data_types,
        entries,
        irreducible,
    })
}

/// Validate one irreducible row of `section`.
fn validate_row(section: Section, row: &IrreducibleRow) -> Result<(), String> {
    if !WHY_NOT_DERIVABLE.contains(&row.why_not_derivable.as_str()) {
        return Err(format!(
            "why_not_derivable {:?} is not one of {WHY_NOT_DERIVABLE:?}",
            row.why_not_derivable
        ));
    }
    let filled = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
    if !(filled(&row.symbol) || filled(&row.pattern) || filled(&row.glob)) {
        return Err("needs a symbol, pattern or glob".into());
    }
    for v in [
        &row.symbol,
        &row.pattern,
        &row.glob,
        &row.activated_by,
        &row.rule,
        &row.bridge,
        &row.value,
    ] {
        if v.as_deref().is_some_and(|s| s.trim().is_empty()) {
            return Err("empty text field".into());
        }
    }
    if row.describe.trim().is_empty() {
        return Err("every row needs `describe`".into());
    }
    if section == Section::FsRoutes && (!filled(&row.glob) || !filled(&row.activated_by)) {
        return Err("fs_routes rows need a glob and the package that activates them".into());
    }
    if section == Section::SyntaxConventions {
        validate_syntax_row(row)?;
    } else if row.rule.is_some() || row.bridge.is_some() || row.value.is_some() {
        return Err("`rule`, `bridge` and `value` belong to syntax_conventions rows".into());
    }
    row.key_sel()?;
    row.handler_sel()?;
    row.verb_sel()?;
    Ok(())
}

/// Validate a `syntax_conventions` row: a known rule, a bridge kind, and the fields the
/// rule's [`SyntaxRuleShape`] needs.
fn validate_syntax_row(row: &IrreducibleRow) -> Result<(), String> {
    let rule = row.rule.as_deref().ok_or("syntax_conventions rows need a `rule`")?;
    let shape = syntax_rule_shape(rule).ok_or_else(|| format!("unknown rule {rule:?}"))?;
    let bridge = row
        .bridge
        .as_deref()
        .ok_or("syntax_conventions rows need a `bridge`")?;
    bridge
        .parse::<trace_core::model::BridgeKind>()
        .map_err(|e| format!("bridge {bridge:?}: {e}"))?;
    let has_symbol = row.symbol.is_some();
    let placeholder = |name: &str| {
        row.pattern
            .as_deref()
            .is_some_and(|p| p.matches(name).count() == 1 && p.len() > name.len())
    };
    let key_arg = matches!(row.key_sel()?, Some(RowSel::Arg(_)));
    let handler_arg = matches!(row.handler_sel()?, Some(RowSel::Arg(_)));
    let ok = match shape {
        SyntaxRuleShape::Symbol => has_symbol,
        SyntaxRuleShape::ServicePattern => placeholder("<Service>"),
        SyntaxRuleShape::SymbolAndFieldPattern => has_symbol && placeholder("<field>"),
        SyntaxRuleShape::SymbolKeyAndHandler => has_symbol && key_arg && handler_arg,
    };
    if ok {
        return Ok(());
    }
    Err(match shape {
        SyntaxRuleShape::Symbol => format!("rule {rule} needs a symbol"),
        SyntaxRuleShape::ServicePattern => {
            format!("rule {rule} needs a pattern with one <Service> placeholder and fixed text")
        }
        SyntaxRuleShape::SymbolAndFieldPattern => {
            format!("rule {rule} needs a symbol and a pattern with one <field> placeholder and fixed text")
        }
        SyntaxRuleShape::SymbolKeyAndHandler => {
            format!("rule {rule} needs a symbol and argument selectors `key` and `handler`")
        }
    })
}

/// Row selector JSON: any argument selector, or `"file_path"`, `"default_export"`,
/// `"exported_names"`.
fn parse_row_selector(v: &Value) -> Result<RowSel, String> {
    match v.as_str() {
        Some("file_path") => Ok(RowSel::FilePath),
        Some("default_export") => Ok(RowSel::DefaultExport),
        Some("exported_names") => Ok(RowSel::ExportedNames),
        _ => parse_selector(v).map(RowSel::Arg),
    }
}

/// Verb JSON (DESIGN §1.12 `"verb": sel`; the same reading as derive's
/// `channels::verb_from_json`): `"any"`, `"exported_names"` (fs_routes), a selector
/// (`{"pos":0}`, `{"field":[1,"method"]}`, `"receiver"`), or an upper-case HTTP verb
/// constant (`"GET"`).
fn parse_verb(v: &Value) -> Result<RowVerb, String> {
    match v {
        Value::String(s) if s == "any" || s == "*" => Ok(RowVerb::Sel(VerbSel::Any)),
        Value::String(s) if s == "exported_names" => Ok(RowVerb::ExportedNames),
        Value::String(s) => match parse_selector(v) {
            Ok(sel) => Ok(RowVerb::Sel(VerbSel::Arg(sel))),
            Err(_) if !s.is_empty() && s.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c == '-') => {
                Ok(RowVerb::Sel(VerbSel::Const(s.clone())))
            }
            Err(_) => Err(format!("unknown verb {v} (a selector, \"any\" or an upper-case constant)")),
        },
        Value::Object(_) => parse_selector(v).map(|s| RowVerb::Sel(VerbSel::Arg(s))),
        _ => Err(format!("unknown verb {v}")),
    }
}

/// Selector JSON: `{"pos":i}`, `{"kw":"k"}`, `{"pos":i,"kw":"k"}`, `{"rest":i}`, `"receiver"`,
/// `"last"`, `{"named_by":i}`, `{"command":i}`, `{"code":i}`,
/// `{"field":[i,"name"]}`, `{"result":true}` (the call's return value).
pub fn parse_selector(v: &Value) -> Result<ArgSel, String> {
    let int = |v: &Value| {
        v.as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| format!("expected a small integer, found {v}"))
    };
    match v {
        Value::String(s) => match s.as_str() {
            "receiver" => Ok(ArgSel::Receiver),
            "last" => Ok(ArgSel::Last),
            other => Err(format!("unknown selector {other:?}")),
        },
        Value::Object(m) => {
            let pos = m.get("pos");
            let kw = m.get("kw").and_then(Value::as_str);
            if m.len() == 2 {
                if let (Some(p), Some(k)) = (pos, kw) {
                    return Ok(ArgSel::PosOrKw(int(p)?, k.to_string()));
                }
            }
            if m.len() != 1 {
                return Err(format!("unknown selector {v}"));
            }
            let (key, val) = m.iter().next().ok_or("empty selector")?;
            match key.as_str() {
                "pos" => Ok(ArgSel::Pos(int(val)?)),
                "kw" => val
                    .as_str()
                    .filter(|k| !k.is_empty())
                    .map(|k| ArgSel::Kw(k.to_string()))
                    .ok_or_else(|| format!("kw must be a non-empty string: {val}")),
                "rest" => Ok(ArgSel::Rest(int(val)?)),
                "named_by" => Ok(ArgSel::NamedBy(int(val)?)),
                "command" => Ok(ArgSel::Command(int(val)?)),
                "code" => Ok(ArgSel::Code(int(val)?)),
                "field" => match val.as_array().map(Vec::as_slice) {
                    Some([i, Value::String(f)]) if !f.is_empty() => Ok(ArgSel::Field {
                        arg: int(i)?,
                        field: f.clone(),
                    }),
                    _ => Err(format!("field needs [arg, \"name\"]: {val}")),
                },
                "result" => match val {
                    Value::Bool(true) => Ok(ArgSel::Result),
                    _ => Err(format!("result must be true: {val}")),
                },
                other => Err(format!("unknown selector key {other:?}")),
            }
        }
        _ => Err(format!("unknown selector {v}")),
    }
}

/// Effect JSON: `{"calls": sel}`, ..., `{"calls_method": {"arg": sel, "method": "run"}}`,
/// `{"copies_members": {"from": sel, "to": sel}}`,
/// `{"delegates_members": {"object": sel, "to": sel}}`, `"receiver"`.
pub fn parse_effect(v: &Value) -> Result<Effect, String> {
    if v.as_str() == Some("receiver") {
        return Ok(Effect::Receiver);
    }
    let m = v
        .as_object()
        .filter(|m| m.len() == 1)
        .ok_or_else(|| format!("unknown effect {v}"))?;
    let (key, val) = m.iter().next().ok_or("empty effect")?;
    let sel = || parse_selector(val);
    Ok(match key.as_str() {
        "calls" => Effect::Calls(sel()?),
        "stored_then_called" => Effect::StoredThenCalled(sel()?),
        "iterates" => Effect::Iterates(sel()?),
        "advances" => Effect::Advances(sel()?),
        "returns" => Effect::Returns(sel()?),
        "wraps" => Effect::Wraps(sel()?),
        "partial" => Effect::Partial(sel()?),
        "property" => Effect::Property(sel()?),
        "never_calls" => Effect::NeverCalls(sel()?),
        "calls_method" => {
            let arg = val
                .get("arg")
                .ok_or_else(|| format!("calls_method needs arg: {val}"))
                .and_then(parse_selector)?;
            let method = val
                .get("method")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .ok_or_else(|| format!("calls_method needs method: {val}"))?;
            Effect::CallsMethod {
                arg,
                method: method.to_string(),
            }
        }
        "copies_members" => {
            let part = |name: &str| {
                val.get(name)
                    .ok_or_else(|| format!("copies_members needs {name}: {val}"))
                    .and_then(parse_selector)
            };
            Effect::CopiesMembers {
                from: part("from")?,
                to: part("to")?,
            }
        }
        "delegates_members" => {
            let part = |name: &str| {
                val.get(name)
                    .ok_or_else(|| format!("delegates_members needs {name}: {val}"))
                    .and_then(parse_selector)
            };
            Effect::DelegatesMembers {
                object: part("object")?,
                to: part("to")?,
            }
        }
        other => return Err(format!("unknown effect {other:?}")),
    })
}

#[cfg(test)]
#[path = "../tests/unit/table.rs"]
mod tests;
