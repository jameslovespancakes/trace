//! Generic derivation rules over the library-mode value-flow IR of installed library files.
//!
//! Per function and parameter, written once for every language (the language adapters only supply
//! language data):
//! * **called**: `p(..)`, through a local alias, an invocation form (`p.call(..)`,
//!   `do.call(p)`, `yield`), a native leaf row (`builtins.map`, `_signal.signal`), or passed
//!   to a library function whose summary calls it (interprocedural fixed point, bounded);
//! * **stored then called**: stored in a slot (self slot, module global, closure container,
//!   container element) whose content some library function calls later;
//! * **wraps**: returned inside a closure / an object whose call method calls it
//!   (**returns**: returned as-is - never a wrapper, the stricter rule);
//! * **iterates**: `for` / comprehension / `yield from` / spread / core element methods;
//! * **property**: stored where a descriptor getter calls it;
//! * **calls a method of it** (`CallsMethod`): a method called on a parameter (or on the slot
//!   it was stored in) whose declared type is an interface or a type without library source
//!   (`handler.ServeHTTP(w, r)` on `handler Handler`): the repository object passed there
//!   runs that method; calling the single method of a functional type (`Runnable.run`) calls
//!   the passed function;
//! * **copies members** (`CopiesMembers`): every member name of one parameter (`for (k in
//!   src)`, the adapter's key functions `Object.getOwnPropertyNames(src)`) is stored /
//!   defined on another parameter with the source's value;
//! * keyword arguments collected by `**kwargs` and forwarded (`super().__init__(**kwargs)`)
//!   reach the callee's parameters of that name (`Kw(name)` effects);
//! * channel effects (PLAN decision 14): `Sends`, `Registers`, `Mounts`, `Decorates`,
//!   `Exports` composing down to the irreducible rows ([`crate::channels`]).
//!
//! The prototype's three precision fixes: (1) receivers inside library code are typed (self,
//! instances of known classes, modules, declared parameter / field types) - an unknown
//! receiver yields nothing, never a method-name union (the repository call's callee comes
//! from the language server); (2) `self` slots are keyed by their class; the slots of two
//! classes share content only when one object can be both (one class is an ancestor of the
//! other, or both are ancestors of one class), and another object's attribute is a slot only
//! when that object is a typed instance; (3) returning a parameter is `Returns`, not a
//! wrapper, and only element iteration counts as iterating.
//! No per-library rule, ever.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use trace_core::config::DeriveSettings;
use trace_core::facts::{
    BindTarget, Expr, FileFacts, FlowFact, ImportKind, ParamKind, Scope, TypeSource, TypeSubject,
};
use trace_core::model::SymbolKind;
use trace_core::text::LineIndex;
use trace_core::Language;
use trace_syntax::lower::{
    self, LibraryOp, CONCAT_CALLEE, CONTAINER_CALLEE, INDEX_READ, KEYS_CALLEE, KEYWORD_SPREAD,
    LITERAL_PREFIX, POSITIONAL_SPREAD,
};

use crate::channels::{rows_of, ChannelRow};
use crate::languages::{self, AdapterSpec};
use crate::model::{ArgSel, Channel, Effect, VerbSel};
use crate::once::OnceMap;
use crate::table::{Section, Tables};

/// Effects of one library function on its parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FunctionSummary {
    /// Library-qualified symbol.
    pub symbol: String,
    /// Qualified name inside its file (`Thread.__init__`), for stub -> implementation lookups.
    #[serde(default)]
    pub qualified: String,
    /// Declaration line (0-based) and byte column of the function name.
    pub line: u32,
    pub column: u32,
    /// Per parameter: its selector and the effects on it.
    pub params: Vec<(ArgSel, Vec<Effect>)>,
    /// Effects that involve several parameters or the returned decorator (channel effects,
    /// `Decorates`).
    #[serde(default)]
    pub effects: Vec<Effect>,
}

impl FunctionSummary {
    /// Effects on the parameter named `name` (keyword-capable selectors carry the name).
    pub fn param_effects(&self, name: &str) -> Vec<Effect> {
        self.params
            .iter()
            .filter(|(sel, _)| selector_name(sel) == Some(name))
            .flat_map(|(_, e)| e.iter().cloned())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.params.is_empty() && self.effects.is_empty()
    }
}

/// The parameter name a selector carries, if any.
fn selector_name(sel: &ArgSel) -> Option<&str> {
    match sel {
        ArgSel::Kw(n) | ArgSel::PosOrKw(_, n) => Some(n),
        _ => None,
    }
}

/// Summaries of every function of one library file, by library-qualified symbol.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileSummaries {
    pub functions: BTreeMap<String, FunctionSummary>,
}

impl FileSummaries {
    /// The function (or class constructor) declared at `line` (0-based), nearest to `column`.
    pub fn at(&self, line: u32, column: u32) -> Option<&FunctionSummary> {
        self.functions
            .values()
            .filter(|f| f.line == line)
            .min_by_key(|f| (i64::from(f.column) - i64::from(column)).abs())
    }

    /// The function whose in-file qualified name is `qualified`.
    pub fn by_qualified(&self, qualified: &str) -> Option<&FunctionSummary> {
        self.functions.values().find(|f| f.qualified == qualified)
    }
}

/// Native leaf facts and irreducible rows the derivation composes with (the embedded
/// [`Tables`] in production; tests supply their own).
pub trait Leaves: Sync {
    /// Effects of a leaf callee without readable source, by library-qualified symbol.
    fn by_symbol(&self, language: Language, symbol: &str, positional: u32) -> Vec<Effect>;
    /// Spelling fallback for leaf callees that resolve to nothing (bare name / qualifier).
    fn by_spelling(
        &self,
        language: Language,
        qualifier: Option<&str>,
        name: &str,
        positional: u32,
    ) -> Vec<Effect>;
    /// Irreducible rows of one section.
    fn rows(&self, language: Language, section: Section) -> Vec<ChannelRow>;
}

impl Leaves for Tables {
    fn by_symbol(&self, language: Language, symbol: &str, positional: u32) -> Vec<Effect> {
        Tables::by_symbol(self, language, symbol)
            .into_iter()
            .filter(|e| e.accepts_arity(positional))
            .flat_map(|e| e.effects.iter().cloned())
            .collect()
    }

    fn by_spelling(
        &self,
        language: Language,
        qualifier: Option<&str>,
        name: &str,
        positional: u32,
    ) -> Vec<Effect> {
        Tables::by_spelling(self, language, qualifier, name, positional)
            .into_iter()
            .flat_map(|e| e.effects.iter().cloned())
            .collect()
    }

    fn rows(&self, language: Language, section: Section) -> Vec<ChannelRow> {
        rows_of(self, language, section)
    }
}

/// Reads library files (read-only; tests use in-memory sources).
pub trait SourceLoader: Sync {
    fn read(&self, path: &Path) -> Option<Vec<u8>>;
}

/// Reads from the file system and from source archives (`<archive>!/<entry>`,
/// [`crate::archive`]); files above `derive.max_file_bytes` (settings) are skipped.
pub struct FsLoader;

impl SourceLoader for FsLoader {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        let max_bytes = trace_core::config::current().derive.max_file_bytes;
        if crate::archive::split(path).is_some() {
            return crate::archive::read(path, max_bytes);
        }
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > max_bytes {
            return None;
        }
        std::fs::read(path).ok()
    }
}

/// One library file as a derivation reads it: its source, syntax facts and library lowering.
struct ParsedFile {
    source: Vec<u8>,
    lines: LineIndex,
    facts: FileFacts,
    low: lower::LibraryLowering,
}

impl ParsedFile {
    /// `None`: the file has no syntax facts or no lowering (it is not loaded).
    fn parse(language: Language, path: &Path, source: Vec<u8>) -> Option<ParsedFile> {
        let path_text = path.to_string_lossy().into_owned();
        let facts = trace_syntax::extract(trace_syntax::SourceInput {
            path: &path_text,
            language,
            source: &source,
        })
        .ok()?;
        let low = lower::lower_library(language, &source, &facts).ok()?;
        Some(ParsedFile {
            lines: LineIndex::new(&source),
            source,
            facts,
            low,
        })
    }
}

/// The library files of one batch of derivations, each read and parsed once (the derivations
/// of one batch load many of the same files): (language, path) -> the parsed file, `None`
/// when it is unreadable or unparsable; the packages read for typed dispatch; resolved imports.
/// Files are kept up to `memory.library_parse_mb` of source (settings); beyond, they are parsed
/// per use. Use one per batch, with one loader and one set of library roots per language.
pub struct ParsedFiles {
    files: OnceMap<(Language, PathBuf), Option<Arc<ParsedFile>>>,
    /// Source bytes kept in `files`, and the bound.
    kept: AtomicUsize,
    budget: usize,
    packages: OnceMap<typed::PackageKey, Arc<typed::Package>>,
    imports: OnceMap<ImportKey, Option<(PathBuf, Option<String>)>>,
}

/// An import to resolve: (language, importing file, target, kind); resolved to the imported
/// file and member.
type ImportKey = (Language, PathBuf, String, ImportKind);

impl Default for ParsedFiles {
    fn default() -> ParsedFiles {
        let mb = trace_core::config::current().memory.library_parse_mb;
        ParsedFiles {
            files: OnceMap::default(),
            kept: AtomicUsize::new(0),
            budget: usize::try_from(mb.saturating_mul(1 << 20)).unwrap_or(usize::MAX),
            packages: OnceMap::default(),
            imports: OnceMap::default(),
        }
    }
}

impl ParsedFiles {
    /// The file at `path`, read with `read` and parsed on first use (while the budget lasts;
    /// then parsed on every use).
    fn get(
        &self,
        language: Language,
        path: &Path,
        read: impl FnOnce() -> Option<Vec<u8>>,
    ) -> Option<Arc<ParsedFile>> {
        let parse = |source: Option<Vec<u8>>| {
            source.and_then(|source| ParsedFile::parse(language, path, source).map(Arc::new))
        };
        let cell = self
            .files
            .cell((language, path.to_path_buf()), self.kept.load(Ordering::Relaxed) < self.budget);
        // Over the budget: parsed for this use only (outside the lock).
        let Some(cell) = cell else { return parse(read()) };
        cell.get_or_init(|| {
            let file = parse(read());
            let bytes = file.as_ref().map_or(0, |f| f.source.len());
            self.kept.fetch_add(bytes, Ordering::Relaxed);
            file
        })
        .clone()
    }
}

/// What a derivation may consult.
pub struct DeriveContext<'a> {
    pub leaves: &'a dyn Leaves,
    pub loader: &'a dyn SourceLoader,
    /// Files already parsed by the derivations of the same batch (same loader).
    pub parsed: &'a ParsedFiles,
    /// Library roots of the language (import resolution, module names).
    pub roots: &'a [PathBuf],
    /// Bounds of the derivation (`derive` settings).
    pub limits: &'a DeriveSettings,
}

/// Derive the summaries of one library file (its imports and namespace group are loaded
/// through `cx`; only the target file's functions are summarised).
pub fn derive_with(language: Language, path: &Path, source: &[u8], cx: &DeriveContext<'_>) -> FileSummaries {
    let Some(spec) = languages::adapter(language) else {
        return FileSummaries::default();
    };
    // The batch's parse of the file when it has these bytes.
    let parsed = match cx.parsed.get(language, path, || Some(source.to_vec())) {
        Some(file) if file.source == source => Some(file),
        _ => ParsedFile::parse(language, path, source.to_vec()).map(Arc::new),
    };
    let Some(parsed) = parsed else {
        return FileSummaries::default();
    };
    let mut program = Program::new(language, spec, cx);
    let root = program.add_unit(path.to_path_buf(), parsed);
    program.load(root);
    program.link();
    let mut st = State::default();
    program.solve(&mut st);
    program.summaries(&mut st, root)
}

// ---------------------------------------------------------------------------------------------
// Program: the loaded library files

const CALLS: u8 = 1;
const STORED: u8 = 2;
const ITERATES: u8 = 4;
const RETURNS: u8 = 8;
const WRAPS: u8 = 16;
const PROPERTY: u8 = 32;
const ALL_EFFECTS: [u8; 6] = [CALLS, STORED, ITERATES, RETURNS, WRAPS, PROPERTY];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum How {
    /// The value itself.
    Direct,
    /// A wrapper that runs the value when called.
    Wrapped,
    /// A value built from it (string concatenation, formatting, unknown calls): flows into
    /// keys, never called.
    Built,
}

/// Abstract values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum V {
    /// Parameter `i` of function `f`.
    Param(u32, u16, How),
    /// An attribute of parameter `i` of function `f` (object use; mount targets).
    ParamAttr(u32, u16),
    /// Content of a slot.
    Slot(u32, How),
    /// A function value (`true`: bound method, the receiver is implicit).
    Fn(u32, bool),
    Inst(u32),
    Class(u32),
    Module(u32),
    /// An interned string literal.
    Lit(u32),
    /// The member names of parameter `i` of function `f` (`Object.keys(p)`, `for (k in p)`).
    Keys(u32, u16),
    /// A record: an object literal naming its entries (fields are `SlotOwner::Record` slots).
    Rec(u32),
}

type Vals = BTreeSet<V>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum SlotOwner {
    Class(u32),
    Unit(u32),
    Local(u32),
    Record(u32),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SlotKey {
    owner: SlotOwner,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Verb {
    Any,
    Const(u32),
    Param(u16),
}

/// Channel / decorator facts of a function (indices are its parameters).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Chan {
    Sends {
        channel: Channel,
        key: u16,
        verb: Verb,
    },
    Registers {
        channel: Channel,
        key: u16,
        handler: u16,
        verb: Verb,
    },
    Mounts {
        key: u16,
        target: u16,
    },
    /// The constructed object's own prefix: routes registered on it are under `key`.
    MountsSelf {
        key: u16,
    },
    /// `handler` registered into the receiver's own registry under the key the receiver
    /// object holds ([`objects`]; composed at the call, never a summary effect).
    RegistersSelf {
        channel: Channel,
        handler: u16,
        verb: Verb,
    },
    /// The returned closure's parameter `param` gets `effect`.
    DecoratesEffect {
        effect: u8,
        param: u16,
    },
    /// The returned closure registers its parameter `handler` under this function's `key`.
    DecoratesRegisters {
        channel: Channel,
        key: u16,
        handler: u16,
        verb: Verb,
    },
}

enum Callee {
    Lib(u32, bool),
    Ctor(u32),
    Value(V),
    Leaf {
        effects: Vec<Effect>,
        symbols: Vec<String>,
    },
}

#[derive(Clone, Debug)]
enum Resolved {
    Func(u32),
    Class(u32),
    Module(u32),
    Var(u32, String),
    /// An import without readable source: its qualified target.
    Leaf(String),
}

#[derive(Clone, Copy, Debug)]
enum Global {
    Func(u32),
    Class(u32),
    Var,
}

struct Imp {
    local: String,
    target: String,
    kind: ImportKind,
    /// Function scope of a function-level import (None = module level).
    scope_func: Option<u32>,
    resolved: Option<(u32, Option<String>)>,
}

struct Unit {
    path: PathBuf,
    module: Option<String>,
    /// Source, line index and syntax facts.
    file: Arc<ParsedFile>,
    decl_func: HashMap<u32, u32>,
    decl_class: HashMap<u32, u32>,
    globals: HashMap<String, Global>,
    imports: Vec<Imp>,
    group: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct PInfo {
    name: String,
    kind: ParamKind,
}

#[derive(Default)]
struct Func {
    unit: u32,
    decl: u32,
    name: String,
    qualified: String,
    is_module: bool,
    is_ctor: bool,
    /// The declaration has a body (interface / abstract methods have none).
    has_body: bool,
    params: Vec<PInfo>,
    self_param: Option<String>,
    class_method: bool,
    class: Option<u32>,
    parent: Option<u32>,
    locals: HashMap<String, Vec<Expr>>,
    container_locals: HashSet<String>,
    field_stores: Vec<(Expr, String, Expr)>,
    member_stores: Vec<(u32, String, Expr)>,
    global_stores: Vec<(String, Expr)>,
    index_stores: Vec<(Expr, Expr, Expr)>,
    evals: Vec<Expr>,
    returns: Vec<Expr>,
    iterates: Vec<Expr>,
    decorated: Vec<(u32, Vec<Expr>)>,
    nested: HashMap<String, u32>,
    /// Classes declared in the function's body (`def f(): class K(Base): ...`).
    local_classes: HashMap<String, u32>,
}

struct Class {
    unit: u32,
    decl: u32,
    qualified: String,
    base_names: Vec<String>,
    bases: Vec<u32>,
    unresolved_bases: Vec<String>,
    methods: HashMap<String, u32>,
    /// Every method of a name (clauses / overloads), in declaration order.
    clauses: HashMap<String, Vec<u32>>,
    nested_classes: HashMap<String, u32>,
    subclasses: Vec<u32>,
    is_interface: bool,
}

struct Program<'c> {
    language: Language,
    spec: &'static AdapterSpec,
    cx: &'c DeriveContext<'c>,
    builtins_module: &'static str,
    units: Vec<Unit>,
    funcs: Vec<Func>,
    classes: Vec<Class>,
    by_path: HashMap<PathBuf, u32>,
    groups: HashMap<PathBuf, Vec<u32>>,
    class_names: HashMap<String, Vec<u32>>,
    io_send: Vec<ChannelRow>,
    io_entry: Vec<ChannelRow>,
    ffi: Vec<ChannelRow>,
    /// Declared types of parameters: (function, parameter index) -> type spelling.
    param_types: HashMap<(u32, u16), String>,
    /// Declared types of fields: (class, field name) -> type spelling.
    field_types: HashMap<(u32, String), String>,
    /// Declared types of local variables: (function, name) -> type spelling.
    local_types: HashMap<(u32, String), String>,
    /// Per class: the other classes one object can be an instance of at the same time
    /// (ancestors, descendants, co-ancestors of a common subclass), sorted.
    related: Vec<Vec<u32>>,
    /// Prototype object model facts ([`objects`]).
    objects: objects::Objects,
}

/// A registration whose key is a parameter of an enclosing function (decorator factories):
/// (enclosing function, its key parameter, handler parameter, channel, verb).
type CapturedRegistration = (u32, u16, u16, Channel, Verb);

#[derive(Default)]
struct State {
    masks: HashMap<(u32, u16), u8>,
    captured: HashMap<u32, BTreeSet<(u32, u16, How, u8)>>,
    slot_keys: Vec<SlotKey>,
    slot_ids: HashMap<SlotKey, u32>,
    slot_mask: Vec<u8>,
    slot_callers: Vec<BTreeSet<u32>>,
    stored: BTreeSet<(u32, u32, V)>,
    /// Classes of instances stored in a slot (typed receivers of values read back out).
    slot_insts: HashMap<u32, BTreeSet<u32>>,
    /// Library classes and functions stored in a slot: calling the slot's content constructs
    /// / calls them (`self.route_class = route_class` with a class default, then
    /// `self.route_class(path, endpoint=f)`).
    slot_callables: HashMap<u32, BTreeSet<V>>,
    owner_stores: BTreeSet<(SlotOwner, u32, u16)>,
    slot_flow: BTreeSet<(u32, u32)>,
    elem_alias: HashMap<(u32, u16), Vals>,
    ret: HashMap<u32, Vals>,
    edges: BTreeSet<(u32, u32)>,
    chan: HashMap<u32, BTreeSet<Chan>>,
    cap_reg: HashMap<u32, BTreeSet<CapturedRegistration>>,
    entries: BTreeMap<u32, Channel>,
    method_alias: HashMap<(u32, String), u32>,
    lits: Vec<String>,
    lit_ids: HashMap<String, u32>,
    dispatched: HashMap<u32, Channel>,
    /// Container slots whose elements have methods called by functions reachable from an
    /// entry (`for r in self.routes: r.matches(scope)`): registries of entry objects.
    method_dispatched: HashMap<u32, Channel>,
    /// Functions calling a method on a slot's elements.
    slot_method_callers: HashMap<u32, BTreeSet<u32>>,
    /// Classes owning a dispatched or method-dispatched slot (registry objects).
    registry_classes: BTreeSet<u32>,
    /// (function, class) -> parameters of the function an instance of the class it constructs
    /// holds (constructor arguments, through factories and other held instances).
    held: HashMap<(u32, u32), BTreeSet<u16>>,
    memo: HashMap<(u32, String), Vals>,
    visiting: HashSet<(u32, String)>,
    changed: bool,
    channels: bool,
    /// Effects on keywords a function collects with `**kwargs` and forwards: (function,
    /// keyword) -> effect mask.
    kw_masks: BTreeMap<(u32, String), u8>,
    /// Methods called on parameters (now or after storing them): (function, parameter,
    /// method).
    pmethods: BTreeSet<(u32, u16, String)>,
    /// Methods called on the content of slots.
    slot_methods: HashMap<u32, BTreeSet<String>>,
    /// Member copies between parameters: (function, from, to).
    copies: BTreeSet<(u32, u16, u16)>,
    /// Prototype object model facts ([`objects`]).
    obj: objects::ObjState,
    /// Typed dispatch ([`typed`]).
    typed: typed::TypedState,
    /// Functions calling a parameter: (function, parameter) -> callers (`derive_params`).
    param_callers: HashMap<(u32, u16), BTreeSet<u32>>,
    /// Slots a parameter is stored into, by the function or its callees (`derive_params`).
    param_slots: HashMap<(u32, u16), BTreeSet<u32>>,
    /// Classes owning a dispatched container of entry objects (`derive_tables`).
    table_owners: BTreeSet<u32>,
    /// Slots whose content reaches the key of a send (sent fields, `derive_procffi`).
    slot_sends: HashMap<u32, BTreeSet<(Channel, Verb)>>,
    /// Records by (unit, literal start).
    records: HashMap<(u32, u32), u32>,
}

impl State {
    fn slot(&mut self, key: SlotKey) -> u32 {
        if let Some(&id) = self.slot_ids.get(&key) {
            return id;
        }
        let id = self.slot_keys.len() as u32;
        self.slot_keys.push(key.clone());
        self.slot_ids.insert(key, id);
        self.slot_mask.push(0);
        self.slot_callers.push(BTreeSet::new());
        id
    }

    fn lit(&mut self, text: &str) -> u32 {
        if let Some(&id) = self.lit_ids.get(text) {
            return id;
        }
        let id = self.lits.len() as u32;
        self.lits.push(text.to_string());
        self.lit_ids.insert(text.to_string(), id);
        id
    }

    fn mask(&self, f: u32, i: u16) -> u8 {
        self.masks.get(&(f, i)).copied().unwrap_or(0)
    }

    fn add_mask(&mut self, f: u32, i: u16, e: u8) {
        let m = self.masks.entry((f, i)).or_insert(0);
        if *m & e != e {
            *m |= e;
            self.changed = true;
        }
    }

    fn add_slot_mask(&mut self, s: u32, e: u8) {
        let m = &mut self.slot_mask[s as usize];
        if *m & e != e {
            *m |= e;
            self.changed = true;
        }
    }

    fn add_caller(&mut self, s: u32, f: u32) {
        if self.slot_callers[s as usize].insert(f) {
            self.changed = true;
        }
    }

    fn add_chan(&mut self, f: u32, c: Chan) {
        if self.chan.entry(f).or_default().insert(c) {
            self.changed = true;
        }
    }

    fn insert_stored(&mut self, s: u32, f: u32, v: V) {
        if self.stored.insert((s, f, v)) {
            self.changed = true;
        }
        match v {
            V::Inst(c) => {
                self.slot_insts.entry(s).or_default().insert(c);
            }
            V::Class(_) | V::Fn(..) => {
                self.changed |= self.slot_callables.entry(s).or_default().insert(v);
            }
            _ => {}
        }
    }

    /// Library classes and functions a slot holds.
    fn callables_of(&self, s: u32) -> Vec<V> {
        self.slot_callables.get(&s).into_iter().flatten().copied().collect()
    }

    /// Classes of the instances a slot holds.
    fn insts_of(&self, s: u32) -> Vec<u32> {
        self.slot_insts.get(&s).into_iter().flatten().copied().collect()
    }

    fn add_kw_mask(&mut self, f: u32, keyword: String, e: u8) {
        let m = self.kw_masks.entry((f, keyword)).or_insert(0);
        if *m & e != e {
            *m |= e;
            self.changed = true;
        }
    }

    fn add_pmethod(&mut self, f: u32, i: u16, method: &str) {
        if self.pmethods.insert((f, i, method.to_string())) {
            self.changed = true;
        }
    }

    fn add_slot_method(&mut self, s: u32, method: &str) {
        if self.slot_methods.entry(s).or_default().insert(method.to_string()) {
            self.changed = true;
        }
    }

    fn add_copy(&mut self, f: u32, from: u16, to: u16) {
        if from != to && self.copies.insert((f, from, to)) {
            self.changed = true;
        }
    }

    /// Methods called on parameter `i` of `f`.
    fn methods_of(&self, f: u32, i: u16) -> Vec<String> {
        self.pmethods
            .range((f, i, String::new())..)
            .take_while(|(g, j, _)| *g == f && *j == i)
            .map(|(_, _, m)| m.clone())
            .collect()
    }
}

/// The last segment of a type spelling without generic arguments, pointer / reference
/// marks and qualification (`*pkg.Handler` -> `Handler`, `java.util.function.Function<T>` ->
/// `Function`).
fn simple_type(type_name: &str) -> &str {
    let base = type_name.split(['<', '[', '(']).next().unwrap_or(type_name);
    let base = base.trim().trim_start_matches(['*', '&']).trim();
    base.rsplit(['.', ':', '\\']).next().unwrap_or(base)
}

/// Composition of an effect on a closure with an effect the closure applies to a captured
/// parameter.
fn compose(outer: u8, inner: u8) -> Option<u8> {
    match outer {
        CALLS => Some(inner),
        STORED => matches!(inner, CALLS | STORED | WRAPS).then_some(STORED),
        RETURNS | WRAPS => matches!(inner, CALLS | STORED | WRAPS).then_some(WRAPS),
        PROPERTY => (inner == CALLS).then_some(PROPERTY),
        _ => None,
    }
}

/// The effect a parameter gets when a value of kind `how` derived from it suffers `e`.
fn eff_for(e: u8, how: How) -> Option<u8> {
    match (e, how) {
        (_, How::Built) => None,
        (CALLS, _) => Some(CALLS),
        (STORED, _) => Some(STORED),
        (WRAPS, _) => Some(WRAPS),
        (ITERATES, How::Direct) => Some(ITERATES),
        (RETURNS, How::Direct) => Some(RETURNS),
        (RETURNS, How::Wrapped) => Some(WRAPS),
        (PROPERTY, How::Direct) => Some(PROPERTY),
        _ => None,
    }
}

fn wrapped(vals: Vals) -> Vals {
    vals.into_iter()
        .filter_map(|v| match v {
            V::Param(f, i, How::Direct | How::Wrapped) => Some(V::Param(f, i, How::Wrapped)),
            V::Slot(s, How::Direct | How::Wrapped) => Some(V::Slot(s, How::Wrapped)),
            V::Fn(..) => Some(v),
            _ => None,
        })
        .collect()
}

fn built(vals: Vals) -> Vals {
    vals.into_iter()
        .filter_map(|v| match v {
            V::Param(f, i, _) => Some(V::Param(f, i, How::Built)),
            V::Slot(s, _) => Some(V::Slot(s, How::Built)),
            V::Lit(_) => Some(v),
            _ => None,
        })
        .collect()
}

fn slots_in(vals: &Vals) -> Vec<u32> {
    vals.iter()
        .filter_map(|v| match v {
            V::Slot(s, How::Direct | How::Wrapped) => Some(*s),
            _ => None,
        })
        .collect()
}

/// Slots a value was built from (`self.prefix + path`).
fn slots_in_any(vals: &Vals) -> Vec<u32> {
    vals.iter()
        .filter_map(|v| match v {
            V::Slot(s, How::Built) => Some(*s),
            _ => None,
        })
        .collect()
}

/// Dotted spelling of a name / attribute chain (`self.x.y`, `table.insert`).
fn spelling(e: &Expr) -> Option<String> {
    match e {
        Expr::Name { name, .. } => Some(name.clone()),
        Expr::Attr { object, attr, .. } => spelling(object).map(|o| format!("{o}.{attr}")),
        _ => None,
    }
}

fn is_container_literal(e: &Expr) -> bool {
    matches!(e, Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Name { name, .. } if name == CONTAINER_CALLEE))
}

/// Arguments a selector picks.
fn select<'e>(
    sel: &ArgSel,
    callee: &'e Expr,
    args: &'e [Expr],
    kwargs: &'e [(String, Expr)],
) -> Vec<&'e Expr> {
    let kw = |k: &str| kwargs.iter().find(|(n, _)| n == k).map(|(_, v)| v);
    match sel {
        ArgSel::Pos(i) => args.get(*i as usize).into_iter().collect(),
        ArgSel::Kw(k) => kw(k).into_iter().collect(),
        ArgSel::PosOrKw(i, k) => args.get(*i as usize).or_else(|| kw(k)).into_iter().collect(),
        ArgSel::Rest(i) => args.iter().skip(*i as usize).collect(),
        ArgSel::Last => args.last().into_iter().collect(),
        ArgSel::Receiver => match callee {
            Expr::Attr { object, .. } => vec![object.as_ref()],
            _ => Vec::new(),
        },
        // A numeric field of a sequence literal is its element (`CFuncPtr((name, lib))`).
        ArgSel::Field { arg, field } if field.bytes().all(|b| b.is_ascii_digit()) => {
            match args.get(*arg as usize) {
                Some(e @ Expr::Call { args: elements, .. }) if is_container_literal(e) => field
                    .parse::<usize>()
                    .ok()
                    .and_then(|k| elements.get(k))
                    .into_iter()
                    .collect(),
                _ => Vec::new(),
            }
        }
        ArgSel::Field { arg, .. } | ArgSel::NamedBy(arg) | ArgSel::Command(arg) | ArgSel::Code(arg) => {
            args.get(*arg as usize).into_iter().collect()
        }
        ArgSel::Result | ArgSel::Member => Vec::new(),
    }
}

impl<'c> Program<'c> {
    fn new(language: Language, spec: &'static AdapterSpec, cx: &'c DeriveContext<'c>) -> Program<'c> {
        Program {
            language,
            builtins_module: trace_syntax::syntax(language)
                .map(|s| s.builtins_module)
                .unwrap_or(""),
            io_send: cx.leaves.rows(language, Section::IoSend),
            io_entry: cx.leaves.rows(language, Section::IoEntry),
            ffi: cx.leaves.rows(language, Section::FfiConventions),
            spec,
            cx,
            units: Vec::new(),
            funcs: Vec::new(),
            classes: Vec::new(),
            by_path: HashMap::new(),
            groups: HashMap::new(),
            class_names: HashMap::new(),
            param_types: HashMap::new(),
            field_types: HashMap::new(),
            local_types: HashMap::new(),
            related: Vec::new(),
            objects: objects::Objects::default(),
        }
    }
}

mod effects;
mod eval;
mod load;
mod methods;
mod names;
mod objects;
mod params;
mod procffi;
mod protocol;
mod solve;
mod summaries;
mod symbols;
mod tables;
pub(crate) mod typed;

#[cfg(test)]
#[path = "../../tests/unit/derive/mod.rs"]
mod tests;
