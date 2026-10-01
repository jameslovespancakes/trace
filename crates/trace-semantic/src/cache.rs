//! Per-file semantic cache keyed by content and dependency hashes.
//!
//! One cache per backend: `<repo cache>/semantic/<backend>.bin` (postcard envelope, magic
//! `TRACESEM`, [`CACHE_SCHEMA`]; always outside the inspected root). For every file it keeps
//! up to [`MAX_ENTRIES_PER_FILE`] results (most recent first) so edits that are reverted and
//! branch switches hit the cache too. An entry is reused only when ALL of these still hold:
//!
//! * the environment key: backend id, tool fingerprint and the configuration files copied
//!   into the backend's snapshots ([`environment_key`]);
//! * the file's content hash and the digest of its syntax facts (owners and spans in the
//!   results are syntax indices; a new extractor changes the digest);
//! * every dependency file (content hash + declaration digest): files owning the targets of
//!   its edges, value references, ambiguous candidates and the implementors of its
//!   implementable members (`FileSemantics::implementations`), and the partition files its
//!   imports resolve to ([`ModuleIndex`]: Python dotted / relative imports, JS/TS relative
//!   specifiers);
//! * the set of files its imports resolve to (a newly added module invalidates importers);
//! * for every unresolved call, the set of partition files declaring the called member name
//!   (a new or removed declaration of that name may resolve the call), and for every
//!   implementable member of the file (interface / trait / abstract members, SPEC section
//!   8.5a) the set of partition files declaring its name (a new implementation elsewhere
//!   re-queries the base file).
//!
//! Documented limitation (same as §5.3): a type change in a module that is neither imported
//! by the file nor owns one of its targets (types flowing through an intermediate module) is
//! only picked up when one of the conditions above changes; deleting the cache directory shown by `trace status` is exact.
//!
//! Shell scripts (bash-language-server scoping, engine rules 2 and 3): a script only sees
//! the declarations of the files it sources, transitively ([`ShellScopes`]). Its import
//! dependencies are that closure, compared by DECLARATION digest only (a body edit of a
//! sourced file changes no answer of the sourcing file), and its name dependencies count
//! only declaring files inside the closure: an edit re-queries only the scripts that source
//! the edited file.
//!
//! Declaration reuse (DESIGN §1.14.1, per-declaration answer reuse): next to the per-file
//! entries the cache keeps the RAW engine answers of each file's latest analysis, split by
//! reuse unit ([`reuse_units`]: outermost named callables) with each unit's body hash
//! ([`FileAnswers`]). When a file changed but its interface fingerprint
//! (`FileFacts::interface`), environment, imports, dependencies and name dependencies are
//! unchanged, [`SemanticCache::reuse`] returns the answers of every unit whose bytes are
//! identical ([`FileReuse`]), shifted by the unit's byte and line offset and with owners
//! re-indexed by uid; the engine then asks the server only for the changed units and the
//! code outside units (module level, class bodies), and implementations (header facts) are
//! reused whole. A unit with a failed request is never reused. Files with syntax errors or
//! invalid UTF-8 are always analysed whole.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trace_core::assemble::symbol_uid;
use trace_core::facts::FileFacts;
use trace_core::fingerprint::PartsHasher;
use trace_core::model::ByteSpan;
use trace_core::semantics::{
    FileSemantics, LibraryFile, SemCallbackParam, SemEdge, SemImplementation, SemLibraryCall,
    SemLibraryDispatch, SemUnresolved, SemValueRef,
};
use trace_core::text::LineIndex;
use trace_core::{Hash32, Language};
use trace_syntax::language_rules::{rules, ModulePaths};

use crate::backend::SemanticFile;
use crate::SemanticError;

/// Envelope magic of cache files.
pub const CACHE_MAGIC: [u8; 8] = *b"TRACESEM";
/// Bump on any change of the cached types or key rules.
/// 2: `FileSemantics::{implementations, resolved_elsewhere}`; implementor and
/// implementable-name dependencies.
/// 3: per-declaration answers ([`FileAnswers`]), shell scope dependencies.
/// 4 (language fixes): `FileSemantics::library_dispatch`, unresolved kinds
/// `template_dependent` / `inactive_code`.
/// 5: `FileSemantics::library_bases`.
/// 6: JEV removed: the `EdgeKind`, `Provider` and `Resolution` variants of cached answers
/// are encoded at new positions.
pub const CACHE_SCHEMA: u32 = 6;
/// Files followed through `source` chains of one script.
pub const MAX_SCOPE_FILES: usize = 10_000;
/// Results kept per file (current and recent versions).
pub const MAX_ENTRIES_PER_FILE: usize = 3;

/// A dependency file at the time the result was computed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheDep {
    pub path: String,
    pub hash: Hash32,
    /// Digest of the file's syntax declarations (target uids).
    pub declarations: Hash32,
}

/// Files declaring an unresolved call's member name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NameDep {
    pub name: String,
    pub declared_in: Hash32,
}

/// A sourced file of a shell script, compared by declarations only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScopeDep {
    pub path: String,
    pub declarations: Hash32,
}

/// Raw engine answers of one reuse unit, in the coordinates of the analysis that produced
/// them (`start` / `line`: the unit's first byte and 1-based line then).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UnitAnswers {
    /// Uid of the unit declaration.
    pub uid: String,
    /// blake3 of the unit's exact bytes.
    pub body: Hash32,
    pub start: u32,
    pub line: u32,
    pub edges: Vec<SemEdge>,
    pub unresolved: Vec<SemUnresolved>,
    pub value_refs: Vec<SemValueRef>,
    pub resolved_elsewhere: Vec<ByteSpan>,
    pub callback_params: Vec<SemCallbackParam>,
    pub library_calls: Vec<(LibraryFile, SemLibraryCall)>,
    /// Callee starts whose analyzer answer also had targets outside the index (stub rule).
    pub incomplete: Vec<u32>,
    /// Diagnostic counts attributed to positions inside the unit.
    pub counts: Vec<(String, u32)>,
    /// Library dispatch answers of calls inside the unit (`FileSemantics::library_dispatch`).
    #[serde(default)]
    pub library_dispatch: Vec<SemLibraryDispatch>,
}

/// Raw engine answers of one file's latest analysis (module docs, declaration reuse).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileAnswers {
    pub env: Hash32,
    /// Content hash of the analysed version.
    pub hash: Hash32,
    /// `FileFacts::interface` of the analysed version.
    pub interface: Hash32,
    pub imports: Hash32,
    pub deps: Vec<CacheDep>,
    pub scope: Vec<ScopeDep>,
    pub names: Vec<NameDep>,
    /// Uid of every declaration of the analysed version (owner indices -> uids).
    pub decl_uids: Vec<String>,
    pub units: Vec<UnitAnswers>,
    /// Implementations of the file's members (base = declaration index then).
    pub implementations: Vec<SemImplementation>,
    /// Diagnostic counts of the implementation / type-hierarchy requests.
    pub implementation_counts: Vec<(String, u32)>,
}

/// Answers of unchanged units, already in the coordinates of the current file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileReuse {
    /// Spans of the reused units: the engine asks nothing inside them.
    pub spans: Vec<ByteSpan>,
    pub edges: Vec<SemEdge>,
    pub unresolved: Vec<SemUnresolved>,
    pub value_refs: Vec<SemValueRef>,
    pub resolved_elsewhere: Vec<ByteSpan>,
    pub callback_params: Vec<SemCallbackParam>,
    pub library_calls: Vec<(LibraryFile, SemLibraryCall)>,
    pub incomplete: Vec<u32>,
    pub library_dispatch: Vec<SemLibraryDispatch>,
    /// (kind, count, position inside the unit).
    pub counts: Vec<(String, u32, u32)>,
    /// Implementations reused whole (`None`: ask again).
    pub implementations: Option<Vec<SemImplementation>>,
    pub implementation_counts: Vec<(String, u32)>,
}

impl FileReuse {
    /// Whether `byte` lies in a reused unit.
    pub fn covers(&self, byte: u32) -> bool {
        self.spans.iter().any(|s| s.start <= byte && byte < s.end)
    }
}

/// Reuse units of a file: outermost named callables (no callable ancestor) with a body.
pub fn reuse_units(facts: &FileFacts) -> Vec<u32> {
    let n = facts.declarations.len();
    (0..n as u32)
        .filter(|&i| {
            let d = &facts.declarations[i as usize];
            if !d.kind.is_callable() || facts.is_synthetic(i) || d.span.bytes.is_empty() {
                return false;
            }
            let mut parent = d.parent;
            let mut steps = 0usize;
            while let Some(p) = parent {
                steps += 1;
                let Some(pd) = facts.declarations.get(p as usize) else { return false };
                if pd.kind.is_callable() || steps > n {
                    return false;
                }
                parent = pd.parent;
            }
            true
        })
        .collect()
}

/// Uid of every declaration of a file (the `#k` occurrence rule of `symbol_uid`).
pub fn decl_uids(path: &str, facts: &FileFacts) -> Vec<String> {
    let mut occurrences: HashMap<&str, u32> = HashMap::new();
    facts
        .declarations
        .iter()
        .map(|d| {
            let k = occurrences.entry(d.qualified_name.as_str()).or_insert(0);
            *k += 1;
            symbol_uid(path, &d.qualified_name, *k)
        })
        .collect()
}

/// Sourced-file scopes of shell scripts (module docs): a `source` target matches every
/// partition file with the same file name (an over-approximation of the server's path
/// resolution, so the scope is never smaller than the server's).
pub struct ShellScopes<'a> {
    by_name: HashMap<&'a str, Vec<&'a str>>,
    facts: HashMap<&'a str, &'a FileFacts>,
}

impl<'a> ShellScopes<'a> {
    pub fn new(files: impl IntoIterator<Item = (&'a str, &'a FileFacts)>) -> Self {
        let mut by_name: HashMap<&'a str, Vec<&'a str>> = HashMap::new();
        let mut facts = HashMap::new();
        for (path, f) in files {
            by_name
                .entry(trace_core::relpath::last_component(path))
                .or_default()
                .push(path);
            facts.insert(path, f);
        }
        for paths in by_name.values_mut() {
            paths.sort_unstable();
        }
        ShellScopes { by_name, facts }
    }

    /// `from` and every file it sources, transitively (bounded by [`MAX_SCOPE_FILES`]).
    pub fn closure(&self, from: &str) -> BTreeSet<&'a str> {
        let mut out: BTreeSet<&'a str> = BTreeSet::new();
        let Some((&start, _)) = self.facts.get_key_value(from) else {
            return out;
        };
        let mut queue: VecDeque<&'a str> = VecDeque::from([start]);
        out.insert(start);
        while let Some(path) = queue.pop_front() {
            let Some(facts) = self.facts.get(path) else { continue };
            for import in &facts.imports {
                let name = trace_core::relpath::last_component(&import.target);
                for &target in self.by_name.get(name).map(Vec::as_slice).unwrap_or_default() {
                    if out.len() >= MAX_SCOPE_FILES {
                        return out;
                    }
                    if out.insert(target) {
                        queue.push_back(target);
                    }
                }
            }
        }
        out
    }
}

/// Last `/` or backslash segment of a path.
/// Imports are sourced script files resolved into one shared scope (shell `source` / `.`).
fn sourced_files(language: Language) -> bool {
    rules(language).modules == ModulePaths::SourcedFiles
}

/// One cached result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub env: Hash32,
    pub hash: Hash32,
    pub facts: Hash32,
    /// Digest of the partition files the file's imports resolve to.
    pub imports: Hash32,
    pub deps: Vec<CacheDep>,
    /// Shell scripts: the sourced closure (declarations only).
    pub scope: Vec<ScopeDep>,
    pub names: Vec<NameDep>,
    pub semantics: FileSemantics,
}

/// Persisted form.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheFile {
    pub backend: String,
    pub files: BTreeMap<String, Vec<CacheEntry>>,
    /// Raw answers of each file's latest analysis (declaration reuse).
    pub answers: BTreeMap<String, FileAnswers>,
}

/// Environment key of a backend: id, tool fingerprint and snapshot configuration files.
pub fn environment_key(
    backend_id: &str,
    tool_fingerprint: &str,
    configs: &[(&str, &[u8])],
    config_names: &[&str],
) -> Hash32 {
    let mut relevant: Vec<(&str, Hash32)> = configs
        .iter()
        .filter(|(path, _)| config_names.iter().any(|p| crate::mirror::glob_match(p, path)))
        .map(|(path, bytes)| (*path, Hash32::of(bytes)))
        .collect();
    relevant.sort();
    let mut h = PartsHasher::new();
    h.text("trace-semantic-cache")
        .int(u64::from(CACHE_SCHEMA))
        .text(trace_core::TRACE_VERSION)
        .text(backend_id)
        .text(tool_fingerprint);
    for (path, hash) in relevant {
        h.text(path).part(&hash.0);
    }
    h.finish()
}

/// Digest of a serializable value (`None` if it cannot be serialized: never cached).
fn digest<T: Serialize + ?Sized>(value: &T) -> Option<Hash32> {
    postcard::to_stdvec(value).ok().map(|bytes| Hash32::of(&bytes))
}

/// Lookup context over the current partition (digests computed lazily, once per run).
pub struct CacheContext<'r, 'a> {
    env: Hash32,
    files: HashMap<&'a str, &'r SemanticFile<'a>>,
    declaring: HashMap<&'a str, Vec<&'a str>>,
    modules: ModuleIndex,
    facts_digests: HashMap<&'a str, Option<Hash32>>,
    decl_digests: HashMap<&'a str, Option<Hash32>>,
    shell: ShellScopes<'a>,
    closures: HashMap<&'a str, BTreeSet<&'a str>>,
}

impl<'r, 'a> CacheContext<'r, 'a> {
    pub fn new(env: Hash32, files: &[&'r SemanticFile<'a>]) -> Self {
        let mut declaring: HashMap<&'a str, Vec<&'a str>> = HashMap::new();
        for f in files {
            let facts: &'a FileFacts = f.facts;
            for d in &facts.declarations {
                declaring.entry(d.name.as_str()).or_default().push(f.path);
            }
        }
        for paths in declaring.values_mut() {
            paths.sort_unstable();
            paths.dedup();
        }
        CacheContext {
            env,
            files: files.iter().map(|f| (f.path, *f)).collect(),
            declaring,
            modules: ModuleIndex::new(files.iter().map(|f| f.path)),
            facts_digests: HashMap::new(),
            decl_digests: HashMap::new(),
            shell: ShellScopes::new(
                files
                    .iter()
                    .filter(|f| sourced_files(f.language))
                    .map(|f| (f.path, f.facts)),
            ),
            closures: HashMap::new(),
        }
    }

    /// Whether `path` is a shell script (sourced-file scoping).
    fn is_shell(&self, path: &str) -> bool {
        self.file(path).is_some_and(|f| sourced_files(f.language))
    }

    /// A shell script and the files it sources, transitively.
    fn scope(&mut self, path: &str) -> BTreeSet<&'a str> {
        let Some(f) = self.file(path) else {
            return BTreeSet::new();
        };
        if let Some(found) = self.closures.get(f.path) {
            return found.clone();
        }
        let closure = self.shell.closure(f.path);
        self.closures.insert(f.path, closure.clone());
        closure
    }

    /// The name digest `path`'s entries use: partition-wide, or within the script's scope.
    fn name_digest_for(&mut self, path: &str, name: &str) -> Hash32 {
        if self.is_shell(path) {
            let scope = self.scope(path);
            return self.name_digest_in(name, Some(&scope));
        }
        self.name_digest_in(name, None)
    }

    /// Content hash of a partition file.
    fn hash_of(&self, path: &str) -> Option<Hash32> {
        self.file(path).map(|f| f.hash)
    }

    /// Shell scope dependencies of `path` (sourced files other than itself).
    fn scope_deps(&mut self, path: &str) -> Option<Vec<ScopeDep>> {
        if !self.is_shell(path) {
            return Some(Vec::new());
        }
        let scope = self.scope(path);
        let mut out = Vec::with_capacity(scope.len());
        for dep in scope {
            if dep == path {
                continue;
            }
            out.push(ScopeDep {
                path: dep.to_string(),
                declarations: self.decl_digest(dep)?,
            });
        }
        Some(out)
    }

    /// Every recorded dependency still holds (content deps, shell scope, names, imports).
    fn deps_hold(
        &mut self,
        path: &str,
        imports: Hash32,
        deps: &[CacheDep],
        scope: &[ScopeDep],
        names: &[NameDep],
    ) -> bool {
        if self.imports_digest(path) != imports {
            return false;
        }
        for dep in deps {
            if self.hash_of(&dep.path) != Some(dep.hash)
                || self.decl_digest(&dep.path) != Some(dep.declarations)
            {
                return false;
            }
        }
        for dep in scope {
            if self.decl_digest(&dep.path) != Some(dep.declarations) {
                return false;
            }
        }
        names
            .iter()
            .all(|n| self.name_digest_for(path, &n.name) == n.declared_in)
    }

    /// Whether the raw answers of `path` may be reused per declaration (module docs).
    fn answers_valid(&mut self, answers: &FileAnswers, path: &str) -> bool {
        let Some(file) = self.file(path) else {
            return false;
        };
        let interface = file.facts.interface;
        if answers.env != self.env
            || file.facts.error_count > 0
            || interface == Hash32::default()
            || answers.interface != interface
            || crate::engine::lsp_text(file.source).is_none()
        {
            return false;
        }
        self.deps_hold(path, answers.imports, &answers.deps, &answers.scope, &answers.names)
    }

    fn file(&self, path: &str) -> Option<&'r SemanticFile<'a>> {
        self.files.get(path).copied()
    }

    fn facts_digest(&mut self, path: &str) -> Option<Hash32> {
        let f = self.file(path)?;
        *self.facts_digests.entry(f.path).or_insert_with(|| digest(f.facts))
    }

    fn decl_digest(&mut self, path: &str) -> Option<Hash32> {
        let f = self.file(path)?;
        *self
            .decl_digests
            .entry(f.path)
            .or_insert_with(|| digest(&f.facts.declarations))
    }

    /// Digest of the sorted set of partition files declaring `name` (within `scope` when
    /// given).
    fn name_digest_in(&self, name: &str, scope: Option<&BTreeSet<&'a str>>) -> Hash32 {
        let mut h = PartsHasher::new();
        h.text(name);
        for path in self.declaring.get(name).map(Vec::as_slice).unwrap_or_default() {
            if scope.is_none_or(|s| s.contains(path)) {
                h.text(path);
            }
        }
        h.finish()
    }

    /// Partition files `path`'s imports resolve to (sorted, without itself); for a shell
    /// script its sourced closure.
    fn import_targets(&mut self, path: &str) -> Vec<&'a str> {
        let Some(f) = self.file(path) else {
            return Vec::new();
        };
        if sourced_files(f.language) {
            return self.scope(path).into_iter().filter(|p| *p != f.path).collect();
        }
        let mut out: BTreeSet<&'a str> = BTreeSet::new();
        for import in &f.facts.imports {
            for target in self.modules.resolve(f.path, f.language, &import.target) {
                if let Some(dep) = self.file(&target) {
                    if dep.path != f.path {
                        out.insert(dep.path);
                    }
                }
            }
        }
        out.into_iter().collect()
    }

    fn imports_digest(&mut self, path: &str) -> Hash32 {
        let mut h = PartsHasher::new();
        for target in self.import_targets(path) {
            h.text(target);
        }
        h.finish()
    }

    fn entry_valid(&mut self, entry: &CacheEntry, path: &str) -> bool {
        let Some(file) = self.file(path) else {
            return false;
        };
        if entry.env != self.env || entry.hash != file.hash || self.facts_digest(path) != Some(entry.facts) {
            return false;
        }
        self.deps_hold(path, entry.imports, &entry.deps, &entry.scope, &entry.names)
    }

    /// Build the entry for fresh results of `path` (`None` when the file is unknown or its
    /// facts cannot be digested).
    fn entry_for(&mut self, path: &str, semantics: &FileSemantics) -> Option<CacheEntry> {
        let file = self.file(path)?;
        let facts = self.facts_digest(path)?;
        let mut dep_paths: BTreeSet<&'a str> = BTreeSet::new();
        let uids = semantics
            .edges
            .iter()
            .map(|e| e.target.as_str())
            .chain(semantics.value_refs.iter().map(|r| r.target.as_str()))
            .chain(
                semantics
                    .unresolved
                    .iter()
                    .flat_map(|u| u.candidates.iter().map(String::as_str)),
            )
            .chain(semantics.implementations.iter().map(|i| i.implementor.as_str()))
            .chain(
                semantics
                    .library_dispatch
                    .iter()
                    .flat_map(|d| d.implementations.iter().map(String::as_str)),
            );
        for uid in uids {
            // A uid is `{path}:{qualified}`; relative paths never contain ':'.
            if let Some(dep) = uid.split_once(':').and_then(|(p, _)| self.file(p)) {
                if dep.path != file.path {
                    dep_paths.insert(dep.path);
                }
            }
        }
        if !self.is_shell(path) {
            dep_paths.extend(self.import_targets(path));
        }
        let scope = self.scope_deps(path)?;
        let mut deps = Vec::with_capacity(dep_paths.len());
        for dep in dep_paths {
            let hash = self.file(dep)?.hash;
            let declarations = self.decl_digest(dep)?;
            deps.push(CacheDep {
                path: dep.to_string(),
                hash,
                declarations,
            });
        }
        let mut names: BTreeSet<&str> = semantics
            .unresolved
            .iter()
            .filter_map(|u| member_name(file.facts, u))
            .collect();
        // Library dispatch answers change when a file starts or stops declaring the member.
        names.extend(semantics.library_dispatch.iter().filter_map(|d| {
            file.facts
                .calls
                .iter()
                .find(|c| c.callee_span == d.at)
                .and_then(|c| c.member.as_deref())
        }));
        names.extend(
            file.facts
                .declarations
                .iter()
                .enumerate()
                .filter(|(i, _)| crate::engine::implementable(file.facts, *i as u32))
                .map(|(_, d)| d.name.as_str()),
        );
        let names: Vec<String> = names.into_iter().map(str::to_string).collect();
        let names = names
            .into_iter()
            .map(|name| NameDep {
                declared_in: self.name_digest_for(path, &name),
                name,
            })
            .collect();
        Some(CacheEntry {
            env: self.env,
            hash: file.hash,
            facts,
            imports: self.imports_digest(path),
            deps,
            scope,
            names,
            semantics: semantics.clone(),
        })
    }
}

/// Called member name of an unresolved entry: the syntax call's member, else the last
/// identifier of the recorded callee text.
fn member_name<'s>(facts: &'s FileFacts, u: &'s SemUnresolved) -> Option<&'s str> {
    facts
        .calls
        .iter()
        .find(|c| c.callee_span == u.at)
        .and_then(|c| c.member.as_deref())
        .or_else(|| {
            u.callee
                .rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .find(|s| !s.is_empty())
        })
}

/// A backend's cache.
#[derive(Debug)]
pub struct SemanticCache {
    path: Option<PathBuf>,
    data: CacheFile,
    dirty: bool,
}

impl SemanticCache {
    /// Load `<dir>/<backend>.bin`. A missing file is an empty cache; an unreadable, corrupt or
    /// foreign file is an empty cache plus a warning.
    pub fn load(dir: &Path, backend: &str) -> (SemanticCache, Option<String>) {
        let path = dir.join(format!("{}.bin", file_stem(backend)));
        let empty = |path: PathBuf| SemanticCache {
            path: Some(path),
            data: CacheFile {
                backend: backend.to_string(),
                files: BTreeMap::new(),
                answers: BTreeMap::new(),
            },
            dirty: false,
        };
        if !path.exists() {
            return (empty(path), None);
        }
        match trace_core::cache::load_blob::<CacheFile>(&path, CACHE_MAGIC, CACHE_SCHEMA) {
            Ok(data) if data.backend == backend => (
                SemanticCache {
                    path: Some(path),
                    data,
                    dirty: false,
                },
                None,
            ),
            Ok(_) => {
                let warning = format!("{} belongs to another backend; ignored", path.display());
                (empty(path), Some(warning))
            }
            Err(e) => {
                let warning = format!("semantic cache discarded ({e})");
                (empty(path), Some(warning))
            }
        }
    }

    /// A cache that is never written to disk.
    pub fn in_memory(backend: &str) -> SemanticCache {
        SemanticCache {
            path: None,
            data: CacheFile {
                backend: backend.to_string(),
                files: BTreeMap::new(),
                answers: BTreeMap::new(),
            },
            dirty: false,
        }
    }

    /// Most recent valid result for `path`.
    pub fn lookup(&self, ctx: &mut CacheContext<'_, '_>, path: &str) -> Option<FileSemantics> {
        self.data
            .files
            .get(path)?
            .iter()
            .find(|entry| ctx.entry_valid(entry, path))
            .map(|entry| entry.semantics.clone())
    }

    /// Record fresh results for `path` (replacing an entry for the same file version).
    pub fn store(&mut self, ctx: &mut CacheContext<'_, '_>, path: &str, semantics: &FileSemantics) {
        let Some(entry) = ctx.entry_for(path, semantics) else {
            return;
        };
        let entries = self.data.files.entry(path.to_string()).or_default();
        entries.retain(|e| !(e.env == entry.env && e.hash == entry.hash && e.facts == entry.facts));
        entries.insert(0, entry);
        entries.truncate(MAX_ENTRIES_PER_FILE);
        self.dirty = true;
    }

    /// Drop files for which `keep` is false (files that left the partition).
    pub fn retain_paths(&mut self, keep: impl Fn(&str) -> bool) {
        let before = self.data.files.len() + self.data.answers.len();
        self.data.files.retain(|path, _| keep(path));
        self.data.answers.retain(|path, _| keep(path));
        self.dirty |= self.data.files.len() + self.data.answers.len() != before;
    }

    /// Per-declaration reuse for a changed `path` (module docs): the answers of every unit
    /// whose bytes are unchanged, in the current file's coordinates. `None` when nothing
    /// can be reused (no answers, another environment or interface, a changed dependency).
    pub fn reuse(&self, ctx: &mut CacheContext<'_, '_>, path: &str) -> Option<FileReuse> {
        let answers = self.data.answers.get(path)?;
        if !ctx.answers_valid(answers, path) {
            return None;
        }
        let file = ctx.file(path)?;
        let reuse = reuse_units_of(answers, file.path, file.source, file.facts);
        (!reuse.spans.is_empty() || reuse.implementations.is_some()).then_some(reuse)
    }

    /// Record the raw answers of `path`'s fresh analysis (dependencies from `semantics`,
    /// the post-processed result stored with [`SemanticCache::store`]).
    pub fn store_answers(
        &mut self,
        ctx: &mut CacheContext<'_, '_>,
        path: &str,
        mut answers: FileAnswers,
        semantics: &FileSemantics,
    ) {
        let Some(entry) = ctx.entry_for(path, semantics) else {
            return;
        };
        answers.env = entry.env;
        answers.imports = entry.imports;
        answers.deps = entry.deps;
        answers.scope = entry.scope;
        answers.names = entry.names;
        self.data.answers.insert(path.to_string(), answers);
        self.dirty = true;
    }

    /// Write the cache atomically if it changed (no-op for in-memory caches).
    pub fn save(&mut self) -> Result<(), SemanticError> {
        let Some(path) = &self.path else {
            self.dirty = false;
            return Ok(());
        };
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        trace_core::cache::save_blob(path, CACHE_MAGIC, CACHE_SCHEMA, &self.data)?;
        self.dirty = false;
        Ok(())
    }
}

/// Translate stored unit answers into the current file (module docs, declaration reuse).
fn reuse_units_of(answers: &FileAnswers, path: &str, source: &[u8], facts: &FileFacts) -> FileReuse {
    let uids = decl_uids(path, facts);
    let index_of: HashMap<&str, u32> = uids.iter().enumerate().map(|(i, u)| (u.as_str(), i as u32)).collect();
    let own_prefix = format!("{path}:");
    // A uid in this file must still exist; uids of other files are unchanged dependencies.
    let known = |uid: &str| !uid.starts_with(&own_prefix) || index_of.contains_key(uid);
    let owner = |old: u32| -> Option<u32> {
        answers
            .decl_uids
            .get(old as usize)
            .and_then(|u| index_of.get(u.as_str()))
            .copied()
    };
    let lines = LineIndex::new(source);
    let old_units: HashMap<&str, &UnitAnswers> = answers.units.iter().map(|u| (u.uid.as_str(), u)).collect();
    let mut reuse = FileReuse::default();
    for unit in reuse_units(facts) {
        let d = &facts.declarations[unit as usize];
        let Some(old) = old_units.get(uids[unit as usize].as_str()) else { continue };
        let span = d.span.bytes;
        let Some(bytes) = source.get(span.range()) else { continue };
        if Hash32::of(bytes) != old.body || old.counts.iter().any(|(k, _)| k == "request_failed") {
            continue;
        }
        let new_line = lines.line1(span.start);
        if let Some(items) = translate_unit(old, span.start, new_line, &owner, &known) {
            reuse.spans.push(span);
            reuse.edges.extend(items.edges);
            reuse.unresolved.extend(items.unresolved);
            reuse.value_refs.extend(items.value_refs);
            reuse.resolved_elsewhere.extend(items.resolved_elsewhere);
            reuse.callback_params.extend(items.callback_params);
            reuse.library_calls.extend(items.library_calls);
            reuse.incomplete.extend(items.incomplete);
            reuse.library_dispatch.extend(items.library_dispatch);
            reuse
                .counts
                .extend(items.counts.into_iter().map(|(k, n)| (k, n, span.start)));
        }
    }
    let implementations: Option<Vec<SemImplementation>> = answers
        .implementations
        .iter()
        .map(|i| {
            let base = owner(i.base)?;
            known(&i.implementor).then(|| SemImplementation {
                base,
                implementor: i.implementor.clone(),
                kind: i.kind,
            })
        })
        .collect();
    if implementations.is_some() {
        reuse.implementation_counts = answers.implementation_counts.clone();
    }
    reuse.implementations = implementations;
    reuse
}

/// One unit's answers moved to `start` / `line` with owners re-indexed; `None` when an
/// owner or an in-file target no longer exists (the unit is asked again).
fn translate_unit(
    old: &UnitAnswers,
    start: u32,
    line: u32,
    owner: &dyn Fn(u32) -> Option<u32>,
    known: &dyn Fn(&str) -> bool,
) -> Option<UnitAnswers> {
    let byte_delta = i64::from(start) - i64::from(old.start);
    let line_delta = i64::from(line) - i64::from(old.line);
    let b = |x: u32| u32::try_from(i64::from(x) + byte_delta).ok();
    let l = |x: u32| u32::try_from(i64::from(x) + line_delta).ok();
    let span = |s: ByteSpan| Some(ByteSpan::new(b(s.start)?, b(s.end)?));
    let mut out = UnitAnswers {
        uid: old.uid.clone(),
        body: old.body,
        start,
        line,
        counts: old.counts.clone(),
        ..UnitAnswers::default()
    };
    for e in &old.edges {
        if !known(&e.target) {
            return None;
        }
        out.edges.push(SemEdge {
            owner: owner(e.owner)?,
            target: e.target.clone(),
            kind: e.kind,
            at: span(e.at)?,
            line: l(e.line)?,
            resolution: e.resolution,
        });
    }
    for u in &old.unresolved {
        if !u.candidates.iter().all(|c| known(c)) {
            return None;
        }
        let owner_new = match u.owner {
            Some(o) => Some(owner(o)?),
            None => None,
        };
        out.unresolved.push(SemUnresolved {
            owner: owner_new,
            kind: u.kind,
            at: span(u.at)?,
            line: l(u.line)?,
            callee: u.callee.clone(),
            candidates: u.candidates.clone(),
        });
    }
    for r in &old.value_refs {
        if !known(&r.target) {
            return None;
        }
        out.value_refs.push(SemValueRef {
            at: span(r.at)?,
            line: l(r.line)?,
            target: r.target.clone(),
        });
    }
    for s in &old.resolved_elsewhere {
        out.resolved_elsewhere.push(span(*s)?);
    }
    for p in &old.callback_params {
        let mut moved = p.clone();
        moved.call = span(p.call)?;
        moved.arg = span(p.arg)?;
        out.callback_params.push(moved);
    }
    for (file, call) in &old.library_calls {
        let mut moved = call.clone();
        moved.at = span(call.at)?;
        moved.line = l(call.line)?;
        out.library_calls.push((file.clone(), moved));
    }
    for p in &old.incomplete {
        out.incomplete.push(b(*p)?);
    }
    for d in &old.library_dispatch {
        if !d.implementations.iter().all(|u| known(u)) {
            return None;
        }
        out.library_dispatch.push(SemLibraryDispatch {
            owner: owner(d.owner)?,
            at: span(d.at)?,
            line: l(d.line)?,
            library_symbol: d.library_symbol.clone(),
            implementations: d.implementations.clone(),
        });
    }
    Some(out)
}

/// File-name-safe backend id (`lsp:gopls` -> `lsp-gopls`).
fn file_stem(backend: &str) -> String {
    backend
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Module-path index of a partition, for import dependencies (an over-approximation is
/// safe: it only causes extra re-queries).
///
/// * Python: a file's module key is its path without `.py`/`.pyi` and without a trailing
///   `/__init__`. An absolute import `a.b.c` depends on every file whose key is `a`, `a/b` or
///   `a/b/c`, or ends with `/<that>` (source roots such as `src/` are not known, so suffixes
///   at a path-segment boundary match). A relative import (`.models.User`) is resolved
///   against the importing file's package first (`..` goes one package up).
/// * JavaScript/TypeScript: relative specifiers (`./x`, `../x`) resolve against the importing
///   file's directory; the key strips the extension (`.js` specifiers also match `.ts`
///   sources) and a trailing `/index`. Package specifiers are external.
#[derive(Debug, Default)]
pub struct ModuleIndex {
    /// Exact module key -> files.
    by_key: HashMap<String, Vec<String>>,
    /// Last key segment -> keys (Python suffix matching).
    by_last: HashMap<String, Vec<String>>,
}

const PY_EXTENSIONS: [&str; 2] = [".pyi", ".py"];
const JS_EXTENSIONS: [&str; 11] = [
    ".d.ts", ".tsx", ".ts", ".mts", ".cts", ".jsx", ".js", ".mjs", ".cjs", ".d.mts", ".d.cts",
];

impl ModuleIndex {
    pub fn new<'p>(paths: impl IntoIterator<Item = &'p str>) -> Self {
        let mut index = ModuleIndex::default();
        for path in paths {
            let Some(key) = module_key(path) else { continue };
            let last = key.rsplit('/').next().unwrap_or(&key).to_string();
            let files = index.by_key.entry(key.clone()).or_default();
            if files.is_empty() {
                index.by_last.entry(last).or_default().push(key);
            }
            files.push(path.to_string());
        }
        index
    }

    /// Files an import of `target` in `from` may load.
    pub fn resolve(&self, from: &str, language: Language, target: &str) -> Vec<String> {
        match rules(language).modules {
            ModulePaths::DottedModules => self.resolve_python(from, target),
            ModulePaths::RelativeSpecifiers => self.resolve_js(from, target),
            _ => Vec::new(),
        }
    }

    fn resolve_python(&self, from: &str, target: &str) -> Vec<String> {
        let dots = target.chars().take_while(|&c| c == '.').count();
        let rest = &target[dots..];
        // `from m import *` records the module itself; `*` is never a path segment.
        let segments: Vec<&str> = rest.split('.').filter(|s| !s.is_empty() && *s != "*").collect();
        let mut out: BTreeSet<String> = BTreeSet::new();
        if dots > 0 {
            // Package of the importing file, then one level up per extra dot.
            let mut base: Vec<&str> = from.split('/').collect();
            base.pop();
            for _ in 1..dots {
                if base.pop().is_none() {
                    return Vec::new();
                }
            }
            if !base.is_empty() {
                out.extend(self.exact(&base.join("/")));
            }
            let mut key = base.join("/");
            for segment in &segments {
                if !key.is_empty() {
                    key.push('/');
                }
                key.push_str(segment);
                out.extend(self.exact(&key));
            }
        } else {
            let mut key = String::new();
            for segment in &segments {
                if !key.is_empty() {
                    key.push('/');
                }
                key.push_str(segment);
                out.extend(self.suffix(&key));
            }
        }
        out.into_iter().collect()
    }

    fn resolve_js(&self, from: &str, target: &str) -> Vec<String> {
        if !(target.starts_with("./") || target.starts_with("../")) {
            return Vec::new();
        }
        let mut out: BTreeSet<String> = BTreeSet::new();
        // `target` is `<specifier>.<export>`; the specifier itself may contain dots.
        let mut candidates = vec![target];
        if let Some((specifier, _)) = target.rsplit_once('.') {
            candidates.push(specifier);
        }
        let dir: Vec<&str> = {
            let mut parts: Vec<&str> = from.split('/').collect();
            parts.pop();
            parts
        };
        for specifier in candidates {
            let mut parts = dir.clone();
            let mut valid = true;
            for part in specifier.split('/') {
                match part {
                    "" | "." => {}
                    ".." => {
                        if parts.pop().is_none() {
                            valid = false;
                            break;
                        }
                    }
                    other => parts.push(other),
                }
            }
            if !valid {
                continue;
            }
            let joined = parts.join("/");
            let key = module_key(&joined).unwrap_or(joined);
            out.extend(self.exact(&key));
        }
        out.into_iter().collect()
    }

    fn exact(&self, key: &str) -> Vec<String> {
        self.by_key.get(key).cloned().unwrap_or_default()
    }

    /// Files whose key equals `key` or ends with `/<key>`.
    fn suffix(&self, key: &str) -> Vec<String> {
        let last = key.rsplit('/').next().unwrap_or(key);
        let mut out = Vec::new();
        for candidate in self.by_last.get(last).map(Vec::as_slice).unwrap_or_default() {
            let matches = candidate == key
                || (candidate.len() > key.len()
                    && candidate.ends_with(key)
                    && candidate.as_bytes()[candidate.len() - key.len() - 1] == b'/');
            if matches {
                out.extend(self.exact(candidate));
            }
        }
        out
    }
}

/// Module key of a source path (`pkg/mod.py` -> `pkg/mod`, `pkg/__init__.py` -> `pkg`,
/// `src/util/index.ts` -> `src/util`); `None` for other files.
fn module_key(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    let stem_len = PY_EXTENSIONS
        .iter()
        .chain(JS_EXTENSIONS.iter())
        .filter(|ext| lower.ends_with(*ext))
        .map(|ext| path.len() - ext.len())
        .min()?;
    let is_python = PY_EXTENSIONS.iter().any(|ext| lower.ends_with(ext));
    let mut key = &path[..stem_len];
    let package_file = if is_python { "__init__" } else { "index" };
    if key == package_file {
        key = "";
    } else if let Some(stripped) = key.strip_suffix(&format!("/{package_file}")) {
        key = stripped;
    }
    Some(key.to_string())
}

#[cfg(test)]
#[path = "../tests/unit/cache.rs"]
mod tests;
