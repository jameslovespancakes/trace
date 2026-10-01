//! Deterministic site inventory with flow callbacks, argument consumption, composition and
//! test provenance.
//!
//! Candidate generation proposes options; it never asserts an edge.
//!
//! 1. dispatch: every proven edge (kind != `passes_callback` / `references`) into a stub
//!    callable whose [`Hierarchy::implementations`] is non-empty. Span = smallest syntax call
//!    callee containing the edge point (else the edge span), activation = the edge kind,
//!    `declared_target` = the stub. id parts: `["dispatch", owner_uid, stub_uid, edge_start_byte]`.
//!    Library-declared dispatch (I-02): every `FileSemantics::library_dispatch` entry (the
//!    server's in-index implementations of a library abstract member called here) and, for
//!    languages whose server answered no such entry anywhere in the index (no
//!    `textDocument/implementation`), every library call whose symbol names a
//!    `<base>.<member>` that repository types implement ([`Hierarchy::library_implementations`],
//!    the family fallback): activation `calls`, `declared_target` None, `declared_library` =
//!    the library symbol. id parts: `["dispatch", owner_uid, "lib:" + symbol, callee_start]`.
//!    A call whose receiver provably is a library-created object (`Index::library_receivers`)
//!    or of a type unrelated to every candidate ([`crate::types`]) is no library dispatch.
//!    Receiver evidence of every dispatch site (I-01, decided by [`crate::decide`]): the
//!    receiver's syntax type ([`crate::types::Types::receiver_type`]) in a statically typed
//!    language, one concrete type whose method is the only one that can run -> proven
//!    (`receiver_exact`, `flow_candidates` = that implementation); a type narrowing the
//!    implementations -> `flow_candidates` (inferred); value flow's override / call targets
//!    at the same call (step 4) join as `flow_candidates` when they come from known receivers
//!    (a class-hierarchy widening is no evidence).
//! 2. callback: every `passes_callback` edge whose argument span matches a syntax
//!    `CallbackArg`, plus every flow callback candidate (a function value — including
//!    lambdas and `functools.partial` wrappers — passed to a consuming parameter or a
//!    library position): span = the receiving call's callee span, candidates =
//!    [target], `argument` = argument text, activation `invoked_callback`. When the
//!    receiving call is a library call, `Site::library` records what the library does with
//!    the argument (`crate::behaviour`; decided by `decide`'s library gate).
//!    id parts: `["callback", owner_uid, target_uid, arg_start_byte]` with the argument span
//!    of the syntax `CallbackArg` the edge / flow argument lies in (both sources share ids,
//!    so a definition-resolved argument and its flow twin are one site, and several edges
//!    inside one argument are one site per target: I-23).
//! 3. no_target: every unresolved `no_semantic_target` with an owner (module-level calls are
//!    owned by the file's `<module>` symbol) whose callee has a member name: candidates =
//!    named callables (anonymous scopes excluded) with that name except the owner (sorted by
//!    uid), filtered by [`crate::narrow`] (name matching, imports, receiver shape, lexical
//!    scope, visibility at the call).
//!    The pool is walked in uid order and collection stops after `MAX_CANDIDATES + 1`
//!    survivors (same result as sorting and truncating the whole pool). Candidates that
//!    share only the name and are not reachable under the language's name rules are
//!    `field_only` (weak). A call whose every name candidate was narrowed away keeps its
//!    site only if flow candidates merge into it (so its id and category stay stable).
//!    id parts: `["no_target", owner_uid, path, start_byte]`.
//! 4. flow/implicit ([`crate::flow`]): a flow `call` / `override_dispatch` candidate on the
//!    same (owner, file, span) as a dispatch site merges into it (union of candidates;
//!    `flow_candidates` only from known receivers, step 1); a flow `call` candidate on the
//!    same (owner, file, span) as a no_target site merges into it (union of candidates, `flow_candidates`,
//!    `field_only`, `test_only`); otherwise a new site (flow `receiver_exact` kept unless
//!    candidates were truncated). Value-flow candidates include decorator calls; the
//!    declared-receiver rules ([`crate::flow::receiver_rule_candidates`]: Rust deref /
//!    `Box` / `&dyn T` / generic-bound receivers -> the trait method, `super` calls -> the
//!    base member) add one candidate per blind call the same way (a rule candidate for a
//!    call already placed as a new flow site joins that site, which is then no longer
//!    `receiver_exact`).
//!    id parts: `[category, owner_uid, path, start_byte, operation]`.
//! 5. composition (appended after all others, `via` = parent site index,
//!    `declared_target` = the parent candidate reached through):
//!    * receiver-specialised flow candidates (e.g. descriptor `__get__` resolving the
//!      function stored in that allocation): category `flow`, operation `call`;
//!      id parts `["flow", owner_uid, path, start_byte, "call", "via", through_uid]`;
//!    * for every non-dispatch, non-no_target site and every candidate that is a stub with
//!      implementations: a dispatch site at the parent's location (id parts as a proven
//!      dispatch site, with the parent's start byte);
//!    * for every callback site and every candidate method overridden in subclasses: a flow
//!      `override_dispatch` site (id parts `["flow", owner_uid, path, start_byte,
//!      "override_dispatch", "via", target_uid]`).
//!
//!    Composition is at most `flow.max_compose_depth` levels deep. A composed site counts only
//!    when its parent decided the declared target ([`crate::decide`]).
//! 6. test provenance: for sites owned by product code, candidates that are test-code
//!    symbols (test declarations, their nested scopes, symbols of test files) and flow
//!    candidates reached only through values originating in test code are `test_only`
//!    (possible tier; never decided).
//!
//! Candidates are truncated to `MAX_CANDIDATES` (sorted by uid, `truncated_candidates`);
//! flow candidates computed from a saturated value-flow slot also set
//! `truncated_candidates` (the set may be incomplete, see `flow_bound`).
//! Site id = first 20 hex of blake3 over the canonical JSON array of the parts.
//! Output order: dispatch, callback, no_target, then new flow/implicit sites, each in
//! (file, start) order; then composed sites in creation order (parents always first).
//!
//! Reference edges (`references`, value uses of callables) are not calls and never seed
//! dispatch sites; the reference design had no such edges (value references were facts).

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{json, Value};
use trace_core::config::FlowSettings;
use trace_core::facts::{ArgSlot, CallSite, CallbackArg};
use trace_core::model::{Diagnostic, LibraryBehaviour, Site, SiteId, SiteOperation};
use trace_core::source::SourceStore;
use trace_core::{
    ByteSpan, EdgeKind, FileId, Index, Language, Location, SiteCategory, SymbolId, SymbolKind, MAX_CANDIDATES,
};
use trace_library::LibraryKnowledge;

use crate::behaviour::{behaviour_for, LibraryCalls, SiteArg};
use crate::flow::{test_symbols, CandidateKind, Flow, FlowCandidate, FlowStats};
use crate::hierarchy::Hierarchy;
use crate::narrow::{is_identifier, Narrower};
use crate::types::{ReceiverType, Types};
use crate::{InferError, LibraryInputs};

mod composition;
mod generator;

use composition::*;
use generator::*;

/// Work counters of one site generation (`TRACE_PROFILE`, scale tests).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SiteStats {
    pub flow: FlowStats,
    /// Flow candidate rows (before placement).
    pub flow_candidates: u64,
    pub sites: u64,
    pub no_target_sites: u64,
    pub dropped_by_import: u64,
    pub dropped_by_receiver: u64,
    pub dropped_by_scope: u64,
    /// Candidates no binding of the call's plain positional arguments accepts (arity).
    pub dropped_by_arity: u64,
    /// Declarations not visible at the call by name (lexical shadowing, non-exported
    /// bindings of other ES modules); value flow may still deliver them.
    pub dropped_by_visibility: u64,
    /// Weak (name-only, unreachable) candidates marked `field_only`.
    pub weak_candidates: u64,
    /// Sites marked truncated because flow read a saturated slot.
    pub bounded_sites: u64,
    /// Callback sites whose receiving call is a library call (`Site::library` set).
    pub library_sites: u64,
    /// Incremental generation reused the previous sites (every input unchanged).
    pub reused: bool,
}

/// Sites plus counters and diagnostics (`flow_bound`) of one generation.
pub struct Generated {
    pub sites: Vec<Site>,
    pub stats: SiteStats,
    pub diagnostics: Vec<Diagnostic>,
    /// Calls whose receiver value comes only from library-created objects, and calls of
    /// parameters only library code runs (value flow, `Flow::library_receivers`; sorted):
    /// the pipeline stores them as `Index::library_receivers`.
    pub library_receivers: Vec<trace_core::LibraryReceiver>,
}

/// Generate all sites for an assembled index (no library knowledge, no injection rules).
pub fn generate(index: &Index, sources: &SourceStore<'_>) -> Result<Vec<Site>, InferError> {
    let hierarchy = Hierarchy::build(index);
    generate_with(index, sources, &hierarchy)
}

/// [`generate`] with a prebuilt hierarchy (the pipeline reuses it for packets).
pub fn generate_with(
    index: &Index,
    sources: &SourceStore<'_>,
    hierarchy: &Hierarchy,
) -> Result<Vec<Site>, InferError> {
    generate_report(index, sources, hierarchy, LibraryInputs::none()).map(|g| g.sites)
}

/// Incremental inference state (PhaseState "sites", version `INFER_VERSION`; PLAN decision
/// 13, DESIGN §1.14.5).
///
/// Site generation reads the whole index (the value-flow solve is one fixpoint over every
/// file: a function stored under an attribute name in one file is called through that name
/// in another), the library knowledge of every call and the active injection rows. The
/// state records a fingerprint of each file's inputs (record content, facts, semantics,
/// library knowledge of its calls), a fingerprint of the global inputs and the generated
/// sites (ids with the uids / paths they name). [`generate_delta`] reuses the sites when
/// every input is unchanged and generates them again otherwise; only the fingerprints of
/// files the delta names are computed again.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SitesState {
    /// Fingerprint of the inputs that are not per file ([`crate::INFER_VERSION`], active
    /// injection rows, the file list).
    pub global: String,
    /// path -> fingerprint of the file's inputs.
    pub files: BTreeMap<String, String>,
    /// The generated sites (ids of the index they were generated for).
    pub sites: Vec<Site>,
    /// uid of every symbol id the stored sites name.
    pub symbols: BTreeMap<u32, String>,
    /// Path of every file id the stored sites name.
    pub paths: BTreeMap<u32, String>,
    /// Diagnostics of the generation (`flow_bound`).
    pub diagnostics: Vec<Diagnostic>,
    /// [`Generated::library_receivers`] (file ids of the index they were generated for,
    /// named in `paths`).
    #[serde(default)]
    pub library_receivers: Vec<trace_core::LibraryReceiver>,
}

/// Fingerprint of everything site generation reads from one file: its record (path,
/// language, content hash, support, pending state, facts, semantics) and the library
/// knowledge of its calls.
fn file_fingerprint(index: &Index, file: FileId, knowledge: &LibraryKnowledge) -> String {
    let record = index.file(file);
    let mut h = blake3::Hasher::new();
    h.update(record.path.as_bytes());
    h.update(&[0]);
    h.update(record.language.as_str().as_bytes());
    h.update(&record.hash.0);
    // Serialisation into the hasher cannot fail for these plain data types; a failure could
    // only make the fingerprint differ (a full generation), never reuse outdated sites.
    let _ =
        serde_json::to_writer(&mut h, &(&record.support, &record.pending, &record.facts, &record.semantic));
    let path = record.path.clone();
    for (key, behaviour) in knowledge.by_call.range((path.clone(), 0)..=(path, u32::MAX)) {
        let _ = serde_json::to_writer(&mut h, &(key, behaviour));
    }
    h.finalize().to_hex().to_string()
}

/// Fingerprint of the inputs that are not per file.
fn global_fingerprint(index: &Index, library: &LibraryInputs<'_>) -> String {
    global_fingerprint_with(index, library, &trace_core::config::current().flow)
}

/// [`global_fingerprint`] under the value-flow settings `flow`: bounds other than the
/// defaults change the sites, so they are part of the key (the default key is unchanged).
fn global_fingerprint_with(index: &Index, library: &LibraryInputs<'_>, flow: &FlowSettings) -> String {
    let mut h = blake3::Hasher::new();
    h.update(&crate::INFER_VERSION.to_le_bytes());
    if *flow != trace_core::config::defaults().flow {
        h.update(format!("{flow:?}").as_bytes());
    }
    for f in &index.files {
        h.update(f.path.as_bytes());
        h.update(&[0]);
    }
    for rule in crate::flow::index_injections(index, library) {
        h.update(format!("{rule:?}").as_bytes());
        h.update(&[0]);
    }
    h.finalize().to_hex().to_string()
}

/// The state of a generation: fingerprints (reused from `prev` for files the delta does not
/// name) and the sites with the uids / paths they name.
fn state_of(
    index: &Index,
    library: &LibraryInputs<'_>,
    prev: Option<&SitesState>,
    delta: &trace_core::delta::IndexDelta,
    generated: &Generated,
) -> SitesState {
    let mut files = BTreeMap::new();
    for (fi, record) in index.files.iter().enumerate() {
        let reused = prev
            .filter(|_| !delta.full && !delta.file_changed(&record.path))
            .and_then(|p| p.files.get(&record.path));
        let fp = match reused {
            Some(fp) => fp.clone(),
            None => file_fingerprint(index, FileId(fi as u32), library.knowledge),
        };
        files.insert(record.path.clone(), fp);
    }
    let mut symbols = BTreeMap::new();
    let mut paths = BTreeMap::new();
    for s in &generated.sites {
        for id in site_symbols(s) {
            symbols.entry(id.0).or_insert_with(|| index.symbol(id).uid.clone());
        }
        paths
            .entry(s.at.file.0)
            .or_insert_with(|| index.file_path(s.at.file).to_string());
    }
    for r in &generated.library_receivers {
        paths
            .entry(r.at.file.0)
            .or_insert_with(|| index.file_path(r.at.file).to_string());
    }
    SitesState {
        global: global_fingerprint(index, library),
        files,
        sites: generated.sites.clone(),
        symbols,
        paths,
        diagnostics: generated.diagnostics.clone(),
        library_receivers: generated.library_receivers.clone(),
    }
}

/// Every symbol id a site names.
fn site_symbols(s: &Site) -> impl Iterator<Item = SymbolId> + '_ {
    std::iter::once(s.owner)
        .chain(s.declared_target)
        .chain(s.candidates.iter().copied())
        .chain(s.flow_candidates.iter().copied())
        .chain(s.field_only.iter().copied())
        .chain(s.test_only.iter().copied())
}

/// The previous sites for the current index when every input is unchanged: the file list
/// and global inputs fingerprint equal, and every file the delta names has the fingerprint
/// it had (files the delta does not name are unchanged by definition). Ids are mapped by
/// uid / path; `None` when anything differs.
fn reusable(
    index: &Index,
    library: &LibraryInputs<'_>,
    prev: &SitesState,
    delta: &trace_core::delta::IndexDelta,
) -> Option<(Vec<Site>, Vec<Diagnostic>, Vec<trace_core::LibraryReceiver>)> {
    if delta.full || !delta.removed.is_empty() || !delta.added.is_empty() {
        return None;
    }
    if prev.files.len() != index.files.len() || prev.global != global_fingerprint(index, library) {
        return None;
    }
    for (fi, record) in index.files.iter().enumerate() {
        let before = prev.files.get(&record.path)?;
        if delta.file_changed(&record.path)
            && *before != file_fingerprint(index, FileId(fi as u32), library.knowledge)
        {
            return None;
        }
    }
    let by_uid: HashMap<&str, SymbolId> = index.symbols.iter().map(|s| (s.uid.as_str(), s.id)).collect();
    let symbol =
        |old: SymbolId| -> Option<SymbolId> { by_uid.get(prev.symbols.get(&old.0)?.as_str()).copied() };
    let file = |old: FileId| -> Option<FileId> { index.file_by_path(prev.paths.get(&old.0)?) };
    let ids = |v: &[SymbolId]| -> Option<Vec<SymbolId>> { v.iter().map(|&s| symbol(s)).collect() };
    let mut sites = Vec::with_capacity(prev.sites.len());
    for s in &prev.sites {
        let mut n = s.clone();
        n.owner = symbol(s.owner)?;
        n.declared_target = match s.declared_target {
            Some(t) => Some(symbol(t)?),
            None => None,
        };
        n.at.file = file(s.at.file)?;
        n.candidates = ids(&s.candidates)?;
        n.flow_candidates = ids(&s.flow_candidates)?;
        n.field_only = ids(&s.field_only)?;
        n.test_only = ids(&s.test_only)?;
        sites.push(n);
    }
    let mut receivers = Vec::with_capacity(prev.library_receivers.len());
    for r in &prev.library_receivers {
        let mut n = r.clone();
        n.at.file = file(r.at.file)?;
        receivers.push(n);
    }
    receivers.sort();
    Some((sites, prev.diagnostics.clone(), receivers))
}

/// Incremental site generation (PLAN decision 13, DESIGN §1.14.5): equal to
/// [`generate_report`] on the same inputs. The previous sites are reused when no input of
/// site generation changed ([`SitesState`]); otherwise every site is generated again (the
/// value-flow fixpoint is global), and the decisions phase re-decides only the sites whose
/// content changed (`decide::decide_delta`). `remap` is not needed: reused sites are
/// mapped by uid and path.
#[allow(clippy::too_many_arguments)]
pub fn generate_delta(
    index: &Index,
    sources: &SourceStore<'_>,
    hierarchy: &Hierarchy,
    library: LibraryInputs<'_>,
    prev: SitesState,
    _remap: &trace_core::delta::IdRemap,
    delta: &trace_core::delta::IndexDelta,
) -> Result<(Generated, SitesState), InferError> {
    if let Some((sites, diagnostics, library_receivers)) = reusable(index, &library, &prev, delta) {
        let stats = SiteStats {
            sites: sites.len() as u64,
            reused: true,
            ..SiteStats::default()
        };
        let generated = Generated {
            sites,
            stats,
            diagnostics,
            library_receivers,
        };
        let state = state_of(index, &library, Some(&prev), delta, &generated);
        return Ok((generated, state));
    }
    let generated = generate_report(index, sources, hierarchy, library)?;
    let state = state_of(index, &library, Some(&prev), delta, &generated);
    Ok((generated, state))
}

/// [`generate_with`] that also returns work counters and `flow_bound` diagnostics.
/// `library`: what library callees do with the functions passed to them (knowledge per
/// call, `Site::library` of callback sites) and the by-name injection rows of the
/// irreducible table with the installed packages that activate them.
pub(crate) fn generate_report(
    index: &Index,
    sources: &SourceStore<'_>,
    hierarchy: &Hierarchy,
    library: LibraryInputs<'_>,
) -> Result<Generated, InferError> {
    let rules = crate::flow::index_injections(index, &library);
    generate_rules(index, sources, hierarchy, library.knowledge, &rules)
}

/// [`generate_report`] with explicit by-name injection rules (the ones the index's
/// `runtime_dispatch` table rows activate) instead of the tables.
pub(crate) fn generate_rules(
    index: &Index,
    sources: &SourceStore<'_>,
    hierarchy: &Hierarchy,
    knowledge: &trace_library::LibraryKnowledge,
    rules: &[crate::flow::Injection],
) -> Result<Generated, InferError> {
    let mut g = Generator {
        index,
        sources,
        hierarchy,
        seen: HashSet::new(),
        tests: test_symbols(index),
        narrower: Narrower::new(index, hierarchy),
        stats: SiteStats::default(),
        callback_spans: HashMap::new(),
        prototypes: index
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::StubImplementation
                    && matches!(&e.provider, trace_core::Provider::Rule(r) if r == "c-prototype")
            })
            .map(|e| (e.from, e.to))
            .collect(),
        rule_start: usize::MAX,
        rejected: HashMap::new(),
        knowledge,
        library_calls: LibraryCalls::new(index),
        types: Types::new(index, hierarchy),
        library_receivers: index
            .library_receivers
            .iter()
            .map(|r| (r.at.file, r.at.bytes))
            .collect(),
    };
    let mut clock = SubPhases::start();
    // The value-flow solve comes first: calls on library-created objects (its
    // `library_receivers`) never become dispatch sites through a library member.
    let flow = Flow::solve_rules(index, hierarchy, knowledge, rules);
    clock.lap("flow_solve");
    let library_receivers = flow.library_receivers();
    g.library_receivers
        .extend(library_receivers.iter().map(|r| (r.at.file, r.at.bytes)));
    clock.lap("library_receivers");
    let mut dispatch = g.dispatch();
    dispatch.extend(g.library_dispatch());
    clock.lap("dispatch");
    let mut callbacks = g.callbacks();
    clock.lap("callbacks");
    let mut no_target = g.no_target();
    clock.lap("no_target");
    let mut candidates = flow.candidates();
    // Declared-receiver rules (deref / wrapper / generic-bound receivers, super calls):
    // one inferred member per blind call, placed like value-flow candidates (no `via`, so
    // the composed-candidate indices above stay valid).
    g.rule_start = candidates.len();
    candidates.extend(crate::flow::receiver_rule_candidates(index, hierarchy));
    clock.lap("flow_candidates");
    if trace_core::config::current().debug.fixtures {
        for c in &candidates {
            eprintln!(
                "flow-candidate: {} {:?} {} {:?}",
                index.symbol(c.owner).uid,
                c.operation,
                c.callee,
                c.candidates
                    .iter()
                    .map(|t| index.symbol(*t).uid.as_str())
                    .collect::<Vec<_>>()
            );
        }
    }
    g.stats.flow = flow.stats().clone();
    g.stats.flow_candidates = candidates.len() as u64;
    let diagnostics = flow.diagnostics();
    drop(flow);
    let placed = g.flow(&candidates, &mut no_target, &mut dispatch);
    clock.lap("flow_sites");
    // Sites whose name candidates were all narrowed away survive only with flow candidates.
    no_target.retain(|s| !s.candidates.is_empty());
    g.stats.no_target_sites = no_target.len() as u64;
    callbacks.extend(placed.callbacks);
    let mut flow_sites = placed.sites;
    for group in [&mut dispatch, &mut callbacks, &mut no_target, &mut flow_sites] {
        group.sort_by_key(|s| (s.at.file, s.at.bytes.start));
    }
    let mut out = Vec::with_capacity(dispatch.len() + callbacks.len() + no_target.len() + flow_sites.len());
    out.extend(dispatch);
    out.extend(callbacks);
    out.extend(no_target);
    out.extend(flow_sites);
    let position: HashMap<SiteId, usize> = out.iter().enumerate().map(|(i, s)| (s.id.clone(), i)).collect();
    g.mark_tests(&mut out);
    let mut depth = vec![0u8; out.len()];
    for c in &candidates {
        let Some((parent, through)) = c.via else {
            continue;
        };
        let Some(&p) = placed.ids[parent].as_ref().and_then(|pid| position.get(pid)) else {
            continue;
        };
        if let Some(site) = g.specialised(c, &out[p], p, through) {
            out.push(site);
            depth.push(1);
        }
    }
    clock.lap("specialised");
    g.compose(&mut out, &mut depth);
    g.mark_tests(&mut out);
    clock.lap("compose");
    g.stats.sites = out.len() as u64;
    g.stats.bounded_sites = candidates.iter().filter(|c| c.bounded).count() as u64;
    Ok(Generated {
        sites: out,
        stats: g.stats,
        diagnostics,
        library_receivers,
    })
}

/// `TRACE_PROFILE=1`: one stderr line per sub-phase of site generation
/// (`profile-infer: <phase> <secs>s`), for bisecting the infer phase on large trees.
struct SubPhases {
    on: bool,
    last: std::time::Instant,
}

impl SubPhases {
    fn start() -> SubPhases {
        SubPhases {
            on: trace_core::env::profile(),
            last: std::time::Instant::now(),
        }
    }

    fn lap(&mut self, phase: &str) {
        if self.on {
            let now = std::time::Instant::now();
            eprintln!("profile-infer: {phase} {:.3}s", (now - self.last).as_secs_f64());
            self.last = now;
        }
    }
}

/// Site id from canonical JSON parts.
pub(crate) fn site_id(parts: &[Value]) -> SiteId {
    let hash = trace_core::fingerprint::hash_json(&Value::Array(parts.to_vec()));
    SiteId(hash.hex_prefix(20))
}

/// Called member of a callee spelling: the last `.`/`::`/`->`
/// segment when it is an identifier; subscripts and calls are not names.
pub(crate) fn member_of(callee: &str) -> Option<&str> {
    let text = callee.trim();
    let tail = [".", "::", "->", "?."]
        .iter()
        .filter_map(|sep| text.rfind(sep).map(|i| i + sep.len()))
        .max()
        .map(|i| &text[i..])
        .unwrap_or(text);
    is_identifier(tail).then_some(tail)
}

/// Sort by uid, dedup and truncate to `MAX_CANDIDATES`.
fn finish_candidates(index: &Index, mut ids: Vec<SymbolId>) -> (Vec<SymbolId>, bool) {
    ids.sort_by(|a, b| index.symbol(*a).uid.cmp(&index.symbol(*b).uid));
    ids.dedup();
    let truncated = ids.len() > MAX_CANDIDATES;
    ids.truncate(MAX_CANDIDATES);
    (ids, truncated)
}

/// Smallest syntax call whose callee span contains `point`.
pub fn call_at(index: &Index, file: FileId, point: u32) -> Option<&CallSite> {
    index
        .file(file)
        .facts
        .as_ref()?
        .calls
        .iter()
        .filter(|c| c.callee_span.contains(point))
        .min_by_key(|c| c.callee_span.len())
}

/// Syntax call whose callee span is exactly `span`.
pub fn call_with_callee(index: &Index, file: FileId, span: ByteSpan) -> Option<&CallSite> {
    index
        .file(file)
        .facts
        .as_ref()?
        .calls
        .iter()
        .find(|c| c.callee_span == span)
}

/// A site with every optional field empty.
#[allow(clippy::too_many_arguments)]
fn site(
    id: SiteId,
    category: SiteCategory,
    owner: SymbolId,
    activation: EdgeKind,
    at: Location,
    callee: String,
    candidates: Vec<SymbolId>,
    truncated: bool,
) -> Site {
    Site {
        id,
        category,
        owner,
        declared_target: None,
        activation,
        at,
        callee,
        candidates,
        flow_candidates: Vec::new(),
        field_only: Vec::new(),
        truncated_candidates: truncated,
        operation: None,
        argument: None,
        via: None,
        test_only: Vec::new(),
        receiver_exact: false,
        library: None,
        declared_library: None,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/sites/mod.rs"]
mod tests;
