//! trace-library: what a library does with a function passed to it (PLAN decision 6).
//!
//! Order of evidence per call site (DESIGN §1.10a): derived from the installed library
//! source ([`derive`], [`languages`], cached per machine in [`cache`], tier set by the
//! precision [`gate`]) -> declared function types (`FileSemantics::callback_params`) -> the
//! tiny native [`table`] -> nothing. Channel effects (PLAN decision 14) come from the same
//! derivation ([`channels`]); [`installed`] lists installed dependency packages for
//! `activated_by` table rows; [`stdindex`] locates stdlib declarations the server gave no
//! location for (only for languages whose server may do that, built lazily on the first such
//! call); [`archive`] reads library source inside `-sources.jar` / `src.zip` archives.
//! Nothing is executed: library files are only read and parsed with syntax trees.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use rayon::prelude::*;
use trace_core::config::DeriveSettings;
use trace_core::semantics::{FnTypeVerdict, LibraryFile};
use trace_core::{Hash32, Language};
use trace_env::{EcosystemId, LibraryKind, LibraryRoot};

use crate::derive::SourceLoader;

pub mod archive;
mod cache;
pub mod channels;
pub mod derive;
pub mod gate;
pub mod injected;
pub mod installed;
pub mod languages;
pub mod library_class;
mod member_hook;
pub mod model;
mod once;
pub mod reflect;
mod stdindex;
pub mod symbol;
pub mod table;

pub use model::*;

/// Version of the derivation engine; part of every summary cache key.
/// 2 (language fixes): `copies_members` / `delegates_members` effects, `ArgSel::Result`.
/// 3: classes and functions held by slots are called / constructed, stores into typed
/// parameters, registrations through the caller's own stores, HTTP method token verbs.
/// 6 (merged derivation packages): JavaScript prototype object model (computed members, `apply` /
/// `call`, `arguments`, mixins); typed dispatch (statically typed registrations, groups), Go
/// receiver methods resolve, variadic handler lists register their last element; import loading
/// by kinship, reachability / stores through parameters, receiver methods over container
/// vocabulary, table-owner configuration and mount-target rules; message protocol rules;
/// process / FFI rules (positional spreads, sent fields, records, base construction without
/// source, function-local classes, `ArgSel::Member`, dynamic symbol lookups are
/// `Sends { ffi }`, Go file name build constraints); reflection metadata.
/// 7: imports of Python stub files resolve to stub modules first; library classes of
/// repository bases (`LibraryKnowledge::classes`) and installed plugin providers
/// (`LibraryKnowledge::providers`).
pub const ENGINE_VERSION: u32 = 7;

/// Library knowledge service: tables, gate, library roots and the per-machine summary cache.
pub struct Library {
    tables: table::Tables,
    gate: gate::Gate,
    cache_dir: PathBuf,
    roots: Vec<LibraryRoot>,
    /// Bounds of derivation (the process's `derive` settings).
    limits: DeriveSettings,
    /// Summaries derived in this process, by implementation file.
    memo: Mutex<HashMap<PathBuf, Arc<derive::FileSummaries>>>,
    /// Readable source of each library location seen in this process (by location path).
    located: Mutex<HashMap<String, Option<(Language, PathBuf)>>>,
    pruned: Mutex<bool>,
}

/// A request's target after stub mapping / stdlib location.
#[derive(Clone, Debug)]
struct Target {
    file: LibraryFile,
    line: u32,
    column: u32,
}

impl Library {
    /// `<cache home>/library/v{ENGINE_VERSION}/` (created on first write).
    pub fn open(cache_home: &Path) -> Result<Library, LibraryError> {
        Ok(Library {
            tables: table::Tables::load_builtin()?,
            gate: gate::Gate::load_builtin()?,
            cache_dir: cache_home.join("library").join(format!("v{ENGINE_VERSION}")),
            roots: Vec::new(),
            limits: trace_core::config::current().derive.clone(),
            memo: Mutex::new(HashMap::new()),
            located: Mutex::new(HashMap::new()),
            pruned: Mutex::new(false),
        })
    }

    /// The library roots the prepared servers use (import resolution inside library code,
    /// stub -> implementation mapping, stdlib module index).
    pub fn with_roots(mut self, roots: Vec<LibraryRoot>) -> Library {
        self.roots = roots;
        self
    }

    /// Replace the precision gate (the gate example evaluates hypothetical gates).
    pub fn with_gate(mut self, gate: gate::Gate) -> Library {
        self.gate = gate;
        self
    }

    pub fn tables(&self) -> &table::Tables {
        &self.tables
    }

    pub fn gate(&self) -> &gate::Gate {
        &self.gate
    }

    /// Directory of the per-machine summary cache.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn roots(&self) -> &[LibraryRoot] {
        &self.roots
    }

    fn root_paths(&self, language: Language) -> Vec<PathBuf> {
        let eco = EcosystemId::of_language(language);
        self.roots
            .iter()
            .filter(|r| Some(r.ecosystem) == eco)
            .map(|r| r.path.clone())
            .collect()
    }

    /// What else summaries of `language` depend on: engine version, embedded tables, roots,
    /// derivation bounds other than the defaults.
    fn context(&self, language: Language) -> Hash32 {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&ENGINE_VERSION.to_le_bytes());
        hasher.update(language.as_str().as_bytes());
        for spec in languages::ALL {
            hasher.update(spec.languages[0].as_str().as_bytes());
            hasher.update(spec.table.as_bytes());
        }
        let mut roots = self.root_paths(language);
        roots.sort();
        for r in roots {
            hasher.update(r.to_string_lossy().as_bytes());
        }
        if self.limits != trace_core::config::defaults().derive {
            hasher.update(format!("{:?}", self.limits).as_bytes());
        }
        Hash32(*hasher.finalize().as_bytes())
    }

    /// Behaviour of every requested call site (order of evidence: derived -> declared type ->
    /// table -> nothing; `never_calls` rows win over the declared type).
    pub fn knowledge(&self, requests: &[BehaviourRequest<'_>]) -> LibraryKnowledge {
        let start = Instant::now();
        let mut knowledge = LibraryKnowledge::default();
        let (by_call, cache_hits) = self.behaviours(requests);
        knowledge.by_call = by_call;
        knowledge.stats.cache_hits = cache_hits;
        knowledge.recount(unique_sites(requests));
        knowledge.stats.seconds = start.elapsed().as_secs_f64();
        knowledge
    }

    /// Behaviours of `requests` keyed by (file, callee start), and the number of summary
    /// cache hits.
    pub fn behaviours(
        &self,
        requests: &[BehaviourRequest<'_>],
    ) -> (BTreeMap<(String, u32), CallBehaviour>, u32) {
        // `TRACE_PROFILE=1`: one `profile-library: <step> <secs>s` line per step.
        let profile = trace_core::env::profile();
        let mut last = Instant::now();
        let mut step = |name: &str| {
            if profile {
                let now = Instant::now();
                eprintln!("profile-library: {name} {:.3}s", (now - last).as_secs_f64());
                last = now;
            }
        };
        // Targets: the server's location (stubs mapped by qualified name), else a unique
        // stdlib module index match.
        let targets: Vec<Option<Target>> = requests.iter().map(|r| self.target_of(r)).collect();
        step("targets");
        let mut files: BTreeMap<(Language, PathBuf), (LibraryFile, PathBuf)> = BTreeMap::new();
        for t in targets.iter().flatten() {
            if let Some((language, implementation)) = self.implementation(&t.file) {
                let mut file = t.file.clone();
                file.language = language;
                files
                    .entry((language, implementation.clone()))
                    .or_insert_with(|| (file, implementation));
            }
        }
        step("implementations");
        // (file key, summaries, cache hit)
        type Derived = ((Language, PathBuf), Option<Arc<derive::FileSummaries>>, bool);
        let parsed = derive::ParsedFiles::default();
        let derived: Vec<Derived> = files
            .par_iter()
            .map(|(key, (file, implementation))| {
                let (summaries, hit) = self.summaries_for(file, implementation, &parsed);
                (key.clone(), summaries, hit)
            })
            .collect();
        let cache_hits = derived.iter().filter(|(_, _, hit)| *hit).count() as u32;
        let summaries: HashMap<(Language, PathBuf), Arc<derive::FileSummaries>> = derived
            .into_iter()
            .filter_map(|(k, s, _)| s.map(|s| (k, s)))
            .collect();
        self.prune_once();
        step(&format!("summaries of {} files ({cache_hits} cached)", files.len()));
        // The requests of each call site in order: the first with a behaviour answers it
        // (call sites in parallel; every library source is parsed once).
        let mut sites: BTreeMap<(String, u32), Vec<usize>> = BTreeMap::new();
        for (i, r) in requests.iter().enumerate() {
            sites.entry((r.file.to_string(), r.callee.start)).or_default().push(i);
        }
        let declarations = Declarations::default();
        let by_call: BTreeMap<(String, u32), CallBehaviour> = sites
            .into_par_iter()
            .filter_map(|(key, at)| {
                at.into_iter()
                    .find_map(|i| {
                        self.behaviour(&requests[i], targets[i].as_ref(), &summaries, &declarations)
                    })
                    .map(|b| (key, b))
            })
            .collect();
        step(&format!("behaviours of {} requests", requests.len()));
        (by_call, cache_hits)
    }

    fn target_of(&self, r: &BehaviourRequest<'_>) -> Option<Target> {
        if let Some((file, line, column)) = r.target {
            return Some(Target {
                file: file.clone(),
                line,
                column,
            });
        }
        // Stdlib module index: the call names its module deterministically and the
        // language's server may answer stdlib calls without a location (never built for
        // languages whose server always locates them).
        let module = r.qualifier?;
        if !languages::adapter(r.language).is_some_and(|s| s.stdlib_index) {
            return None;
        }
        let mut found: Vec<(LibraryFile, u32, u32)> = Vec::new();
        for root in self.stdlib_roots(r.language) {
            let index = stdindex::StdIndex::load_or_build(Some(&self.cache_dir), r.language, root);
            if let Some((path, line, column)) = index.lookup(module, r.spelling, Some(r.positional_args)) {
                found.push((stdindex::stdlib_file(r.language, root, path), line, column));
            }
        }
        match found.as_slice() {
            [(file, line, column)] => Some(Target {
                file: file.clone(),
                line: *line,
                column: *column,
            }),
            _ => None,
        }
    }

    fn stdlib_roots(&self, language: Language) -> impl Iterator<Item = &LibraryRoot> {
        let eco = EcosystemId::of_language(language);
        self.roots
            .iter()
            .filter(move |r| r.kind == LibraryKind::Stdlib && Some(r.ecosystem) == eco)
    }

    /// The source derivation reads for a library location, and its language: the file itself
    /// when the server reported readable source; else the readable form of the location -
    /// a stub's implementation (`.pyi` -> sibling `.py` or the interpreter's module; `.d.ts`
    /// -> the package's implementation file), a class file's source in a sources archive.
    /// `None`: no source is installed.
    fn implementation(&self, file: &LibraryFile) -> Option<(Language, PathBuf)> {
        if let Some(found) = self.located.lock().ok().and_then(|m| m.get(&file.path).cloned()) {
            return found;
        }
        let found = self.locate_implementation(file);
        if let Ok(mut m) = self.located.lock() {
            m.insert(file.path.clone(), found.clone());
        }
        found
    }

    fn locate_implementation(&self, file: &LibraryFile) -> Option<(Language, PathBuf)> {
        let path = PathBuf::from(&file.path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let found = if let Some(stem) = name.strip_suffix(".pyi") {
            let sibling = path.with_file_name(format!("{stem}.py"));
            if sibling.is_file() {
                Some(sibling)
            } else {
                self.typeshed_implementation(&path)
            }
        } else if let Some(mapped) = languages::adapter(file.language)
            .and_then(|a| (a.source_of_location)(&file.path, &self.root_paths(file.language)))
        {
            Some(mapped)
        } else if file.readable && path.is_file() {
            Some(path)
        } else {
            None
        }?;
        // An implementation in another language of the family (`.d.ts` -> `.js`) is derived
        // with that language's grammar.
        let language = trace_core::languages::from_path(&found)
            .filter(|l| languages::adapter(*l).is_some())
            .unwrap_or(file.language);
        Some((language, found))
    }

    /// A typeshed stub (`.../stdlib/<module path>.pyi` or `.../stubs/<dist>/<module path>.pyi`)
    /// -> the module in a Python library root.
    fn typeshed_implementation(&self, stub: &Path) -> Option<PathBuf> {
        let parts: Vec<String> = stub
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let (rel, kind): (Vec<String>, LibraryKind) =
            if let Some(i) = parts.iter().rposition(|c| c == "stdlib") {
                (parts[i + 1..].to_vec(), LibraryKind::Stdlib)
            } else {
                let i = parts.iter().rposition(|c| c == "stubs")?;
                (parts.get(i + 2..)?.to_vec(), LibraryKind::Dependency)
            };
        let mut rel: PathBuf = rel.iter().collect();
        let is_init = rel.file_name().is_some_and(|n| n == "__init__.pyi");
        if is_init {
            rel.pop();
        } else {
            rel.set_extension("");
        }
        self.roots
            .iter()
            .filter(|r| r.ecosystem == EcosystemId::Python && r.kind == kind)
            .find_map(|r| {
                let base = r.path.join(&rel);
                let mut file = base.clone();
                file.set_extension("py");
                [file, base.join("__init__.py")].into_iter().find(|p| p.is_file())
            })
    }

    /// Summaries of one implementation file: process memo, per-machine cache, else derived
    /// (and cached). The flag says whether the per-machine cache answered. `parsed`: the
    /// library files the batch's derivations have parsed.
    fn summaries_for(
        &self,
        file: &LibraryFile,
        implementation: &Path,
        parsed: &derive::ParsedFiles,
    ) -> (Option<Arc<derive::FileSummaries>>, bool) {
        if let Some(found) = self.memo.lock().ok().and_then(|m| m.get(implementation).cloned()) {
            return (Some(found), true);
        }
        let Some(bytes) = derive::FsLoader.read(implementation) else {
            return (None, false);
        };
        let key = cache::SummaryKey {
            language: file.language,
            package: file.package.clone(),
            version: file.version.clone(),
            file_hash: Hash32::of(&bytes),
            context: self.context(file.language),
        };
        let store = cache::SummaryCache::open(&self.cache_dir);
        let (summaries, hit) = match store.get(&key) {
            Some(s) => (s, true),
            None => {
                let roots = self.root_paths(file.language);
                let cx = derive::DeriveContext {
                    leaves: &self.tables,
                    loader: &derive::FsLoader,
                    parsed,
                    roots: &roots,
                    limits: &self.limits,
                };
                let s = derive::derive_with(file.language, implementation, &bytes, &cx);
                // A cache that cannot be written only costs a re-derivation next time.
                let _ = store.put(&key, &s);
                (s, false)
            }
        };
        let summaries = Arc::new(summaries);
        if let Ok(mut m) = self.memo.lock() {
            m.insert(implementation.to_path_buf(), summaries.clone());
        }
        (Some(summaries), hit)
    }

    fn prune_once(&self) {
        let Ok(mut done) = self.pruned.lock() else { return };
        if !*done {
            *done = true;
            cache::SummaryCache::open(&self.cache_dir).prune(self.limits.cache_max_bytes);
        }
    }

    /// The summary of the callee declaration:
    /// * located in the derived source itself: the summary at the declaration position, else
    ///   the declaration at that position by its qualified name (every clause of a function
    ///   is one summary);
    /// * located in a stub (`.pyi`, `.d.ts`): the stub declaration's qualified name in the
    ///   implementation, else its name when exactly one implementation function has it;
    /// * located in a class file without a source position (jdtls `jdt://` locations): the
    ///   server-given class symbol and the called member (`<class>.<member>`; a call named
    ///   like the class is its constructor).
    fn summary_for<'s>(
        &self,
        r: &BehaviourRequest<'_>,
        target: &Target,
        implementation: &Path,
        summaries: &'s derive::FileSummaries,
        declarations: &Declarations,
    ) -> Option<&'s derive::FunctionSummary> {
        let file = &target.file;
        if implementation == Path::new(&file.path) {
            if let Some(found) = summaries.at(target.line, target.column) {
                return Some(found);
            }
            let qualified =
                declarations.name_at(&file.path, file.language, true, target.line, target.column)?;
            return summaries.by_qualified(&qualified.0);
        }
        if let Some((qualified, name)) =
            declarations.name_at(&file.path, file.language, false, target.line, target.column)
        {
            if let Some(found) = summaries.by_qualified(&qualified) {
                return Some(found);
            }
            let mut same = summaries
                .functions
                .values()
                .filter(|f| f.qualified.rsplit('.').next() == Some(name.as_str()));
            let first = same.next();
            if same.next().is_none() {
                if let Some(found) = first {
                    return Some(found);
                }
            }
        }
        summary_by_symbol(r, summaries)
    }

    fn behaviour(
        &self,
        r: &BehaviourRequest<'_>,
        target: Option<&Target>,
        summaries: &HashMap<(Language, PathBuf), Arc<derive::FileSummaries>>,
        declarations: &Declarations,
    ) -> Option<CallBehaviour> {
        let language = r.language;
        let summary = target.and_then(|t| {
            let (implementation_language, implementation) = self.implementation(&t.file)?;
            let s = summaries.get(&(implementation_language, implementation.clone()))?;
            self.summary_for(r, t, &implementation, s, declarations)
                .cloned()
                .map(|found| member_hook::at_call(implementation_language, found, r.spelling))
        });
        let symbol: Option<String> = r
            .symbol
            .map(str::to_string)
            .or_else(|| summary.as_ref().map(|s| s.symbol.clone()));
        let entries: Vec<&table::TableEntry> = symbol
            .as_deref()
            .map(|s| {
                self.tables
                    .by_symbol(language, s)
                    .into_iter()
                    .filter(|e| e.accepts_arity(r.positional_args))
                    .collect()
            })
            .unwrap_or_default();
        let never_calls = entries.iter().any(|e| e.never_calls());
        // 1. Derived from installed source.
        if let (Some(s), Some(t)) = (&summary, target) {
            let effects = relevant_effects(s, r);
            if !effects.is_empty() {
                let version = t.file.version.as_deref().map(|v| format!("@{v}")).unwrap_or_default();
                return Some(CallBehaviour {
                    symbol: symbol.clone(),
                    effects,
                    source: BehaviourSource::Derived,
                    inferred: self.gate.passed(language),
                    reason: format!("derived from the installed source of {}{version}", t.file.package),
                });
            }
        }
        // 2. Declared function types (a `never_calls` row overrides them).
        if !never_calls {
            let mut effects = Vec::new();
            let mut reason = String::new();
            let mut declared_symbol = None;
            for p in &r.callback_params {
                if p.verdict != FnTypeVerdict::FunctionType {
                    continue;
                }
                let Some(arg) = r.args.iter().find(|a| a.span == p.arg) else {
                    continue;
                };
                let Some(sel) = arg_selector(arg, p.param_name.as_deref()) else {
                    continue;
                };
                effects.push(Effect::Calls(sel));
                if reason.is_empty() {
                    reason = match &p.param_name {
                        Some(name) => {
                            format!("parameter {name} is declared as a function type ({})", p.param_type)
                        }
                        None => format!("the parameter is declared as a function type ({})", p.param_type),
                    };
                }
                declared_symbol = declared_symbol.or_else(|| p.library_symbol.clone());
            }
            if !effects.is_empty() {
                return Some(CallBehaviour {
                    symbol: symbol.clone().or(declared_symbol),
                    effects,
                    source: BehaviourSource::DeclaredType,
                    inferred: true,
                    reason,
                });
            }
        }
        // 3. Native table: by the resolved symbol; by spelling only for callees that resolve
        //    to nothing (no location, no symbol).
        let entries: Vec<&table::TableEntry> = if !entries.is_empty() {
            entries
        } else if target.is_none() && r.symbol.is_none() {
            self.tables
                .by_spelling(language, r.qualifier, r.spelling, r.positional_args)
        } else {
            Vec::new()
        };
        if let Some(first) = entries.first() {
            let mut effects: Vec<Effect> = Vec::new();
            for e in &entries {
                for effect in &e.effects {
                    if !effects.contains(effect) {
                        effects.push(effect.clone());
                    }
                }
            }
            return Some(CallBehaviour {
                symbol: symbol.or_else(|| Some(first.symbol.clone())),
                effects,
                source: BehaviourSource::Table,
                inferred: true,
                reason: first.describe.clone(),
            });
        }
        None
    }
}

/// Declaration lookups of one [`Library::behaviours`] call: each library source is read and
/// parsed once (by path, language and read bound) for every request located in it.
#[derive(Default)]
struct Declarations {
    parsed: Mutex<HashMap<(String, Language, bool), Arc<Parsed>>>,
}

/// A library source parsed at most once (`None`: unreadable or unparsable).
type Parsed = OnceLock<Option<ParsedSource>>;

/// A library source and its syntax facts.
struct ParsedSource {
    source: Vec<u8>,
    facts: trace_core::facts::FileFacts,
}

impl Declarations {
    /// (qualified name, name) of the declaration at (line, column) of the library source
    /// `path`; `bounded`: read the way derivation reads ([`derive::FsLoader`]), else the whole
    /// file.
    fn name_at(
        &self,
        path: &str,
        language: Language,
        bounded: bool,
        line: u32,
        column: u32,
    ) -> Option<(String, String)> {
        let slot = self
            .parsed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry((path.to_string(), language, bounded))
            .or_default()
            .clone();
        let parsed = slot
            .get_or_init(|| {
                let file = Path::new(path);
                let source = if bounded {
                    derive::FsLoader.read(file)
                } else {
                    std::fs::read(file).ok().filter(|_| file.is_file())
                }?;
                let facts = trace_syntax::extract(trace_syntax::SourceInput {
                    path,
                    language,
                    source: &source,
                })
                .ok()?;
                Some(ParsedSource { source, facts })
            })
            .as_ref()?;
        let index = symbol::declaration_at(&parsed.facts, &parsed.source, line, column)?;
        let decl = &parsed.facts.declarations[index];
        Some((decl.qualified_name.clone(), decl.name.clone()))
    }
}

/// The summary of `<symbol>.<member>` (`$` of nested JVM classes read as `.`), or of the
/// symbol itself when the call is named like it (a constructor call).
fn summary_by_symbol<'s>(
    r: &BehaviourRequest<'_>,
    summaries: &'s derive::FileSummaries,
) -> Option<&'s derive::FunctionSummary> {
    let symbol = r.symbol?.replace('$', ".");
    if let Some(found) = summaries.functions.get(&format!("{symbol}.{}", r.spelling)) {
        return Some(found);
    }
    let simple = symbol.rsplit(['.', ':', '\\', '/']).next()?;
    if simple == r.spelling {
        return summaries.functions.get(&symbol);
    }
    None
}

/// Distinct (file, callee start) keys of the requests.
pub fn unique_sites(requests: &[BehaviourRequest<'_>]) -> u32 {
    let keys: std::collections::BTreeSet<(&str, u32)> =
        requests.iter().map(|r| (r.file, r.callee.start)).collect();
    keys.len() as u32
}

/// Selector of a request argument (keyword arguments by name; positional ones keyword-capable
/// when the declared parameter name is known).
fn arg_selector(arg: &RequestArg, param_name: Option<&str>) -> Option<ArgSel> {
    match (arg.index, &arg.keyword, param_name) {
        (Some(i), _, Some(name)) => Some(ArgSel::PosOrKw(i, name.to_string())),
        (Some(i), _, None) => Some(ArgSel::Pos(i)),
        (None, Some(k), _) => Some(ArgSel::Kw(k.clone())),
        (None, None, _) => None,
    }
}

/// The derived effects that matter at a call site: effects on the parameters that receive
/// the call's function arguments; methods the callee calls on any argument the call passes
/// (objects passed to an interface-typed parameter); member copies / delegation between
/// arguments; channel / decorator effects.
fn relevant_effects(summary: &derive::FunctionSummary, r: &BehaviourRequest<'_>) -> Vec<Effect> {
    let mut out: Vec<Effect> = Vec::new();
    let passed = |sel: &ArgSel| {
        (0..r.positional_args).any(|i| sel.picks(Some(i), None))
            || r.keywords.iter().any(|k| sel.picks(None, Some(*k)))
            || r.args.iter().any(|a| sel.picks(a.index, a.keyword.as_deref()))
    };
    for (sel, effects) in &summary.params {
        let function_argument = r.args.iter().any(|a| sel.picks(a.index, a.keyword.as_deref()));
        for e in effects {
            let wanted = function_argument || (matches!(e, Effect::CallsMethod { .. }) && passed(sel));
            if wanted && !out.contains(e) {
                out.push(e.clone());
            }
        }
    }
    for e in &summary.effects {
        let wanted = e.is_channel()
            || matches!(
                e,
                Effect::Decorates { .. } | Effect::CopiesMembers { .. } | Effect::DelegatesMembers { .. }
            );
        if wanted && !out.contains(e) {
            out.push(e.clone());
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/unit/support/mod.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
