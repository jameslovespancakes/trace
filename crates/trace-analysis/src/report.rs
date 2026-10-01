//! JSON output contract for every command (`--json`). SPEC §10 documents each shape; every
//! query report carries `"schema": 1` ([`SCHEMA`]). Text rendering in trace-cli is derived
//! from these same structs.

use serde::Serialize;
use trace_core::{Language, SupportLevel};

/// Version of the JSON report schema (`Envelope::schema`).
pub const SCHEMA: u32 = 1;

/// Common envelope fields, flattened into every report.
#[derive(Clone, Debug, Serialize)]
pub struct Envelope {
    /// `show` | `uses` | `deps` | `path` | `context`.
    pub command: &'static str,
    /// JSON schema version ([`SCHEMA`]).
    pub schema: u32,
    pub trace_version: &'static str,
    pub root: String,
    pub include: &'static str,
    pub index: IndexInfo,
    pub seconds: f64,
    /// Tiers that actually occur in this result (`proven`, `inferred`, `possible`), in order.
    pub tiers_used: Vec<&'static str>,
    /// Bridge edges were traversed (always true from the CLI; `OpenOptions::no_bridges`
    /// sets false).
    pub bridges: bool,
    /// Whether the answer can replace searching (`uses` / `deps`; `None` for commands
    /// without a completeness notion).
    pub completeness: Option<Completeness>,
}

/// "Is anything missing?" (NEXT.md item 5). `status`:
/// * `complete`: every syntax occurrence of the target's name (calls, references,
///   imports, re-exports, declarations) in indexed files was resolved: to the target family,
///   or proven to be something else;
/// * `partial`: some occurrences are unresolved; each is listed in `unresolved` (tier
///   `possible`) so the caller checks just those;
/// * `unknown`: the name occurs in files without syntax facts or the query hit a bound.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Completeness {
    pub status: &'static str,
    /// One line, e.g. `complete: all 12 name matches resolved (...)` or `partial: 12
    /// same-name sites unresolved (8 calls, 3 reads, 1 import; 7 in files not analyzed yet)`
    /// (`deps`: `partial: 3 calls inside unresolved (listed as possible)`).
    pub summary: String,
    /// Syntax occurrences of the name considered.
    pub name_matches: usize,
    /// Occurrences resolved to the target family.
    pub resolved_to_target: usize,
    /// Occurrences proven to resolve elsewhere (other symbol, library, builtin).
    pub resolved_elsewhere: usize,
    /// Unresolved same-name sites (tier `possible`), ranked (`UnresolvedMatch::rank`):
    /// same module first, then files importing the target's file / module, then name-only
    /// matches. `uses`: every site (the text output shows 20 plus `(+N more)`); `deps`: at
    /// most `MAX_UNRESOLVED`.
    pub unresolved: Vec<UnresolvedMatch>,
    /// Pending languages with name matches (not analysed yet: set up on first use; their
    /// matches are `possible`, "not analyzed: ..." in the summary).
    pub pending_languages: Vec<Language>,
    /// Files with name matches that are not analysed yet (pending languages / sub-projects).
    pub pending_files: usize,
    /// Files with name matches the server reported as outside the build on this machine.
    pub outside_build_files: usize,
    /// `resolved_elsewhere` broken down by why (keys sorted): `server` (a language server
    /// resolved the site to another symbol or proved it external), `local_binding` (the
    /// name denotes a local / parameter binding: lexical shadowing), `unrelated_type` (member
    /// access on a receiver whose declared / constructed type is provably outside the
    /// target's family), `member_binding` (a member access `obj.x` of a free / exported
    /// function `x`, or a bare name of a member, where the language defines no such
    /// binding), `other_declaration` (another symbol's declaration), `other_language` (the
    /// occurrence's language cannot name the family without a bridge), `library_object` (a
    /// call on an object only a library created), `other_scope` (a nearer binding of the file
    /// denotes the name), `arity` (no family member accepts the call's positional arguments).
    /// Only counts > 0 appear.
    pub elsewhere_reasons: std::collections::BTreeMap<&'static str, usize>,
}

/// A same-name occurrence nothing resolved.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UnresolvedMatch {
    pub at: At,
    /// Executing symbol (uid), incl. `<module>` / `<lambda>` owners.
    pub owner: Option<String>,
    /// `call` | `read` | `write` | `import` | `reexport` | `declaration`.
    pub kind: &'static str,
    /// Exact source line (trimmed, <= 240 chars).
    pub text: String,
    /// `no_semantic_target` | `external_or_ambiguous` | `unresolved_signature` |
    /// `not_analyzed` | `outside_build` | `possible_only` | `inferred_elsewhere` | `bounded`.
    pub reason: &'static str,
    /// 1-based position in the ranked list (`uses`; `deps` ranks by (file, line)).
    pub rank: u32,
    /// Rank group: `same_module` (the target's file, or its package / namespace / module
    /// directory where the language defines one), `imports_target` (a file that imports the
    /// target's file / module or the target itself), `name_only` (everything else).
    pub scope: &'static str,
}

/// One numbered candidate of an ambiguous selector (SPEC 9.4 / 10 errors). Callers retry
/// with the exact `id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CandidateRef {
    /// 1-based position in the candidate list.
    pub n: usize,
    pub id: String,
    pub kind: &'static str,
    pub file: String,
    pub line: u32,
}

/// Both ends of a bridge edge (`EdgeRow::bridge`, `uses` evidence).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BridgeInfo {
    /// `http`, `pyo3`, `jni`, ... (`EdgeRow::kind` is `bridge:<kind>`).
    pub kind: &'static str,
    pub label: String,
    pub from_language: Language,
    pub to_language: Language,
    /// The `to`-side evidence (registration / attribute / export / declaration).
    pub to_at: At,
    pub assumptions: Vec<String>,
    /// Contract file path, when declared by one.
    pub contract: Option<String>,
    pub candidates: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct IndexInfo {
    pub fresh: bool,
    /// `none` | `incremental` | `full` — what this invocation did to keep the index fresh.
    pub updated: &'static str,
    pub files: usize,
    pub symbols: usize,
    pub fingerprint: String,
}

/// A symbol as shown in every output.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Card {
    /// Stable uid (`path:Qualified.name[#k]`); valid CLI reference.
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub file: String,
    pub kind: &'static str,
    pub language: Language,
    pub line: u32,
    pub end_line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    /// First docstring line, <= 160 chars.
    pub summary: String,
    pub semantic: bool,
}

/// Evidence location.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct At {
    pub file: String,
    pub line: u32,
    pub start_byte: u32,
    pub end_byte: u32,
}

/// Decision summary attached to inferred/possible edges.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DecisionInfo {
    pub site: String,
    pub category: &'static str,
    pub status: &'static str,
    pub reason: Option<String>,
}

/// An edge in outputs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EdgeRow {
    pub from: String,
    pub to: String,
    pub kind: &'static str,
    pub tier: &'static str,
    /// Provider label (`pyright`, `typescript`, `rust-analyzer`, `lsp:gopls`, ...).
    pub source: String,
    pub resolution: &'static str,
    pub at: At,
    pub decision: Option<DecisionInfo>,
    /// Present for `bridge:<kind>` edges.
    pub bridge: Option<BridgeInfo>,
    /// Exact source line of `at` (terminator removed, <= 400 chars; empty when the file is
    /// unreadable).
    pub text: String,
    /// Display facts of the site (`path` hops): the call on one line, its conditions and
    /// the values it passes; null elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<SiteInfo>,
}

/// Display facts of a call / use site, read from the syntax tree at query time
/// (`trace_syntax::callsite`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SiteInfo {
    /// The call expression on one line (comments removed, line breaks collapsed); empty
    /// for a site that is not a call.
    pub call: String,
    /// Conditions under which the site runs inside its function, outermost first.
    pub when: Vec<String>,
    /// Values the call passes: `argument -> parameter` of the callee.
    pub carries: Vec<Carry>,
}

/// One value passed by a call: the argument expression and the callee parameter it binds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Carry {
    pub argument: String,
    pub parameter: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BoundsInfo {
    pub hit: Vec<&'static str>,
    pub complete: bool,
    pub work: u64,
    pub depth: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UnresolvedRow {
    pub owner: Option<String>,
    pub kind: &'static str,
    pub callee: String,
    pub at: At,
    pub candidates: usize,
    /// Exact source line of the call site (terminator removed, <= 400 chars; empty when the
    /// file is unreadable).
    pub text: String,
}

/// A reached symbol with its strongest connecting tier and BFS distance.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reached {
    #[serde(flatten)]
    pub card: Card,
    pub tier: &'static str,
    pub distance: u32,
    /// Edge kind that first reached it.
    pub via: &'static str,
    /// The symbol whose edge reached it (one step closer to the start; the start itself at
    /// distance 1), uid; `None` when no edge is known.
    pub from: Option<String>,
    /// Evidence location of that reaching edge (its call site in `from`).
    pub at: Option<At>,
}

// ---------------------------------------------------------------- deps
#[derive(Clone, Debug, Serialize)]
pub struct DependenciesReport {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub symbol: Card,
    /// `--deep`: every level of `then` and the unresolved sites of every reached symbol.
    pub deep: bool,
    /// Every site inside the symbol that links out (one row per site, source order), with
    /// its condition and targets; unresolved calls of the symbol are rows without targets.
    pub calls: Vec<CallRow>,
    pub results: Vec<Reached>,
    pub edges: Vec<EdgeRow>,
    pub unresolved_inside: Vec<UnresolvedRow>,
    pub bounds: BoundsInfo,
    pub notice: &'static str,
}

/// One site inside a `deps` / `context` symbol.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CallRow {
    pub at: At,
    /// `call` | `read` | `write` | `callback` | `import` | `bridge` | ... (row kind).
    pub kind: &'static str,
    /// Strongest tier among the targets; `possible` for an unresolved call.
    pub tier: &'static str,
    /// The call on one line (the exact line text for a non-call site).
    pub call: String,
    /// Conditions under which the site runs inside the symbol, outermost first.
    pub when: Vec<String>,
    /// Targets (a dispatch site may have several); empty = unresolved.
    pub targets: Vec<CallTarget>,
    /// A dispatch / flow site at this call is still undecided: which implementation runs is
    /// not known, its candidates are listed as `possible` targets and the call counts as
    /// unresolved (`deps` is never `complete` with it).
    pub undecided: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CallTarget {
    pub id: String,
    pub file: String,
    pub line: u32,
    pub tier: &'static str,
}

// ---------------------------------------------------------------- path
#[derive(Clone, Debug, Serialize)]
pub struct PathRow {
    pub nodes: Vec<String>,
    /// Language of every node (aligned with `nodes`): rows show it where it changes.
    pub languages: Vec<Language>,
    pub edges: Vec<EdgeRow>,
    /// Weakest tier on the path.
    pub tier: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct PathReport {
    #[serde(flatten)]
    pub envelope: Envelope,
    /// `--deep`: every bounded path (shortest first) instead of one shortest path.
    pub deep: bool,
    pub from: Card,
    pub to: Card,
    pub found: bool,
    pub paths: Vec<PathRow>,
    pub bounds: BoundsInfo,
    /// No path found: undecided sites reachable from `from` whose candidates can reach `to`
    /// (a path may run through them; `no path  <N> unresolved`); 0 otherwise.
    pub unresolved: usize,
    /// No path found in the selected view, but one exists through `possible` edges (a
    /// candidate call or a cross-language link below the view): `trace path --deep` shows it.
    pub possible_path: bool,
    pub note: &'static str,
}

// ---------------------------------------------------------------- uses --deep (impact)
#[derive(Clone, Debug, Serialize)]
pub struct CallerRow {
    #[serde(flatten)]
    pub card: Card,
    pub tier: &'static str,
    pub relation: &'static str,
    pub distance: u32,
    pub target: String,
    pub reason: String,
    pub call_site: Option<At>,
    /// Exact text of the call-site line (terminator removed, <= 400 characters); empty when
    /// there is no call site.
    pub text: String,
    /// Family member (override / implementation) the site resolves to when it is not a
    /// target itself.
    pub via: Option<String>,
    /// Transitive rows: uid of the distance-1 caller their first-edge chain passes (`None`
    /// for direct rows, declarations, and when the chain breaks).
    pub through: Option<String>,
}

/// Section sizes of [`DeepImpact`] (`call_sites` counts rows, `callers` distinct symbols).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ImpactTotals {
    pub callers: usize,
    pub call_sites: usize,
    pub other_references: usize,
    pub transitive: usize,
    pub tests: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResultUseRow {
    pub caller: String,
    pub tier: &'static str,
    pub call: String,
    pub line: u32,
    /// Grouped phrases, e.g. `reads attribute (2x, e.g. line 40: user.id)`.
    pub uses: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResultUses {
    pub target: String,
    pub callers: Vec<ResultUseRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SimilarRow {
    #[serde(flatten)]
    pub card: Card,
    pub similarity: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SimilarCode {
    pub target: String,
    pub matches: Vec<SimilarRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TestRow {
    /// `path::name` (Python-style) for every language.
    pub test: String,
    pub file: String,
    pub line: u32,
    pub mentions: String,
    /// `direct` | `via_caller`.
    pub directness: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnknownInfo {
    /// Unresolved call sites inside the targets (no proven target).
    pub unresolved_inside: Vec<UnresolvedRow>,
    /// Undecided edge-case sites whose candidates include a target.
    pub undecided_sites_into_targets: usize,
    /// Pending languages present in the index (not analysed yet; callers there are missing
    /// until a query sets them up).
    pub pending_languages: Vec<Language>,
    /// Traversal bounds hit while collecting callers.
    pub bounds: BoundsInfo,
}

/// The deep sections of `uses --deep`: direct callers, other references, transitive
/// callers, result uses, similar code, tests and unknowns (the family is top-level in
/// [`UsesReport`]).
#[derive(Clone, Debug, Serialize)]
pub struct DeepImpact {
    /// Direct call sites (distance 1) of the target and its family: one row per call site
    /// (`via` = the family member when it is not the target).
    pub callers: Vec<CallerRow>,
    /// Non-call uses of the target and its family (reads, writes, imports, re-exports,
    /// callbacks) plus the declarations that change with it: `relation` is the use kind.
    pub other_references: Vec<CallerRow>,
    /// Transitive callers (distance >= 2), capped at 200; `transitive_total` is uncapped.
    /// `through` names the distance-1 caller each chain passes.
    pub transitive: Vec<CallerRow>,
    pub transitive_total: usize,
    pub totals: ImpactTotals,
    pub result_uses: Vec<ResultUses>,
    pub similar_code: Vec<SimilarCode>,
    pub tests: Vec<TestRow>,
    pub unknown: UnknownInfo,
}

// ---------------------------------------------------------------- uses
/// One use of the target (NEXT.md item 7). Sorted by (file, line, column).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReferenceRow {
    /// `call` | `read` | `write` | `import` | `reexport` | `callback` | `override` |
    /// `implements` | `declaration` | `bridge`.
    pub kind: &'static str,
    pub tier: &'static str,
    pub file: String,
    pub language: Language,
    pub line: u32,
    /// 1-based column of `start_byte` in characters.
    pub column: u32,
    pub start_byte: u32,
    pub end_byte: u32,
    /// Exact text of the whole line (trailing newline removed, <= 400 chars).
    pub text: String,
    /// Executing symbol (uid) containing the use.
    pub owner: Option<String>,
    /// Family member the use actually resolves to, when not the target itself
    /// (`via override src/flask/json/provider.py:DefaultJSONProvider.dumps`).
    pub via: Option<String>,
    /// `index` (stored edges or query-time family rules). `name_match` rows are no longer
    /// produced: possible sites are only in `completeness.unresolved` (SPEC §10.3).
    pub source: String,
    /// How the row was established: the edge's resolution (`call_hierarchy`,
    /// `definition`, `inheritance_rule`, `implementation`, `import_path`,
    /// `deterministic_unique`, ...), `family_overloads` /
    /// `family_flow` for query-time family rows, `declaration` for declaration rows.
    pub resolution: &'static str,
    /// Conditions under which the use runs inside its function, outermost first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub when: Vec<String>,
}

/// Row counts of a `uses` report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsesCounts {
    /// Rows whose kind is not `declaration`.
    pub uses: usize,
    /// Rows of kind `override` or `implements`.
    pub overrides: usize,
    /// Rows of kind `declaration`.
    pub declarations: usize,
    /// `completeness.unresolved.len()`.
    pub unresolved: usize,
    /// `--deep`: `impact.transitive_total`, else null.
    pub callers_of_callers: Option<usize>,
    /// `--deep`: `impact.tests.len()`, else null.
    pub tests: Option<usize>,
}

/// Full provenance of the target's own links (provider, resolution, decisions, bridges).
#[derive(Clone, Debug, Serialize)]
pub struct UsesEvidence {
    pub incoming: Vec<EvidenceRow>,
    pub outgoing: Vec<EvidenceRow>,
    /// Sites owned by the symbol.
    pub sites: Vec<SiteRow>,
    /// Unresolved call sites owned by the symbol.
    pub unresolved: Vec<UnresolvedRow>,
}

/// `uses <symbol> [--deep]`: every use, the provenance of the target's links and, with
/// `--deep`, callers of callers and tests.
#[derive(Clone, Debug, Serialize)]
pub struct UsesReport {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub symbol: Card,
    /// Override / implementation family (target excluded; stub counterparts included).
    /// Uses that resolve to a member carry `via`.
    pub family: Vec<Card>,
    pub deep: bool,
    /// Sorted by (file, line, column).
    pub uses: Vec<ReferenceRow>,
    pub counts: UsesCounts,
    pub evidence: UsesEvidence,
    /// Entry points above the uses and the tests that matter (always).
    pub summary: UsesSummary,
    /// `--deep` sections, else null.
    pub impact: Option<DeepImpact>,
}

/// The short impact of a change: where the uses are reached from and which tests matter.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UsesSummary {
    /// Per direct caller (non-test code): the entry points above it (callers nothing else in
    /// product code calls; the caller itself when it is one).
    pub impact: Vec<ImpactChain>,
    /// Tests that set an option a use depends on (`router.Debug = true` for a use guarded
    /// by `engine.Debug`) first, then tests that mention the symbol or its callers.
    pub tests: Vec<GuardTest>,
    /// Every relevant test (`tests_index`) plus the option-setting tests.
    pub tests_total: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImpactChain {
    /// Direct caller (uid).
    pub caller: String,
    /// Entry points above it (uids), at most [`crate::queries::sites::ENTRY_POINTS_SHOWN`];
    /// `entry_points_total` counts all.
    pub entry_points: Vec<String>,
    pub entry_points_total: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct GuardTest {
    /// `path::name`.
    pub test: String,
    pub file: String,
    pub line: u32,
    /// The line that sets the option (`router.RedirectFixedPath = true`); empty for a test
    /// listed because it mentions the symbol.
    pub sets: String,
}

// ---------------------------------------------------------------- evidence rows
#[derive(Clone, Debug, Serialize)]
pub struct EvidenceRow {
    #[serde(flatten)]
    pub edge: EdgeRow,
    pub other: Card,
}

#[derive(Clone, Debug, Serialize)]
pub struct SiteRow {
    pub id: String,
    pub category: &'static str,
    pub operation: Option<&'static str>,
    pub callee: String,
    pub at: At,
    pub candidates: Vec<String>,
    pub truncated_candidates: bool,
    pub decision: DecisionInfo,
    pub decided_targets: Vec<String>,
}

// ---------------------------------------------------------------- show
/// One symbol's exact source.
#[derive(Clone, Debug, Serialize)]
pub struct ShowItem {
    pub symbol: Card,
    /// The declaration's exact bytes (hash-verified; never cut).
    pub source: String,
    /// Distinct callers (symbols) at the evidence tier.
    pub callers: usize,
    /// Sites inside the symbol that link out (calls, reads, ... and unresolved calls).
    pub calls: usize,
}

/// `show <symbol>...`: exact source of each symbol.
#[derive(Clone, Debug, Serialize)]
pub struct ShowReport {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub symbols: Vec<ShowItem>,
}

// ---------------------------------------------------------------- context
/// A call site of the context symbol.
#[derive(Clone, Debug, Serialize)]
pub struct CallerSite {
    /// Calling symbol (uid).
    pub caller: String,
    pub at: At,
    pub tier: &'static str,
    /// Exact line text.
    pub text: String,
    pub when: Vec<String>,
    /// Values bound to the symbol's parameters here (`argument -> parameter`), when the
    /// argument differs from the parameter name.
    pub carries: Vec<Carry>,
}

/// `context <symbol>`: the symbol's source and everything around it.
#[derive(Clone, Debug, Serialize)]
pub struct ContextReport {
    #[serde(flatten)]
    pub envelope: Envelope,
    /// `--deep`: callers of callers and every level below the calls.
    pub deep: bool,
    pub symbol: Card,
    /// Exact source of the symbol.
    pub source: String,
    pub callers: Vec<CallerSite>,
    /// Sites inside the symbol (as `deps`).
    pub calls: Vec<CallRow>,
    /// `--deep`: callers of the callers (uids, BFS order); empty otherwise.
    pub callers_of_callers: Vec<String>,
    /// Symbols reached below the direct calls (`deps` `then`): distance 2, every level with
    /// `--deep`.
    pub below: Vec<Reached>,
    pub tests: Vec<GuardTest>,
    pub tests_total: usize,
    /// Commands worth running next (exact selectors).
    pub next: Vec<String>,
}

// ---------------------------------------------------------------- status
#[derive(Clone, Debug, Serialize)]
pub struct LanguageRow {
    pub language: Language,
    pub files: u32,
    pub support: SupportLevel,
    pub backend: Option<String>,
    pub backend_available: bool,
    pub reason: String,
    /// Call resolution of the language's analysed files (`trace status` only; null
    /// elsewhere and without call sites).
    pub resolution: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EdgeCounts {
    pub proven: usize,
    pub inferred: usize,
    pub possible: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SiteCounts {
    pub total: usize,
    pub decided: usize,
    pub undecided: usize,
    pub by_category: std::collections::BTreeMap<&'static str, usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct IndexHealth {
    pub exists: bool,
    pub fresh: Option<bool>,
    pub stale_files: Vec<String>,
    pub files: usize,
    pub symbols: usize,
    pub edges: EdgeCounts,
    pub unresolved: usize,
    pub sites: SiteCounts,
    pub omitted: usize,
    pub built_unix: Option<f64>,
    pub schema: u32,
    pub full_builds: u32,
    pub incremental_updates: u32,
    pub cache_bytes: u64,
    /// Files not analysed yet (pending languages and sub-projects, set up on first use).
    pub pending_files: usize,
    /// Dependents left stale by an interface change (updated before the next answer).
    pub stale: usize,
    /// Files the servers reported as outside the build on this machine.
    pub outside_build_files: usize,
    /// Files with failed server requests (their calls stay unknown).
    pub request_failed_files: usize,
}

/// Hit rate of one cache (`hit_rate` is null before the first lookup).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CacheRate {
    pub lookups: u64,
    pub hits: u64,
    pub hit_rate: Option<f64>,
}

/// Cache hit rates accumulated in `<repo cache>/stats.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CacheRates {
    /// Per-file semantic results reused by index updates.
    pub semantic_files: CacheRate,
}

/// Semantic analyzer configuration.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SemanticInfo {
    /// Maximum analyzer processes per backend (files are sharded across them; Pyright picks
    /// K <= this from the file count and `memory_budget_mb`).
    pub pool_size: usize,
    /// Setting `memory.budget_mb` (0 = unbounded).
    pub memory_budget_mb: u64,
    /// Analyzer sessions stay alive between updates in these modes.
    pub persistent_sessions_in: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PendingRow {
    pub language: Language,
    pub files: usize,
    /// Why they are not analysed yet (pending language, or `sub-project <dir>: ...`).
    pub reason: String,
}

/// Library behaviour coverage (PLAN launch targets): callback sites passing a function to a
/// library callee whose behaviour trace determined itself (derived from installed source,
/// declared function types, or the native table) / all such sites.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LibraryBehaviourStatus {
    /// Callback sites whose receiving callee is outside the repository.
    pub sites: usize,
    pub derived: usize,
    pub declared_type: usize,
    pub table: usize,
    /// `(derived + declared_type + table) / sites`, 3 decimals; null without sites.
    pub coverage: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AreaRow {
    pub area: String,
    pub files: usize,
    pub functions: usize,
    pub classes: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct HubRow {
    #[serde(flatten)]
    pub card: Card,
    pub callers: usize,
}

/// Overview data: areas, entry points, hubs.
#[derive(Clone, Debug, Serialize)]
pub struct Overview {
    pub areas: Vec<AreaRow>,
    pub entry_points: Vec<Card>,
    pub hubs: Vec<HubRow>,
}

/// Resolved vs unresolved call sites of one language (NEXT.md item 6).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolutionHealth {
    pub language: Language,
    pub support: SupportLevel,
    /// Syntax call sites with a proven or inferred target (in the index or proven external).
    pub resolved: usize,
    /// Call sites without one (`no_semantic_target`, `no_semantic_backend`,
    /// `unresolved_signature`, ambiguous in-index targets, or nothing recorded).
    pub unresolved: usize,
    /// `resolved / (resolved + unresolved)`, 3 decimals; null without call sites.
    pub rate: Option<f64>,
    /// `low_resolution` when a semantic language resolves less than
    /// `analysis.resolution_warn` of its *in-repository* call sites (`in_repo_rate`).
    pub warning: Option<&'static str>,
    /// Call sites whose called name matches a declaration of the repository (same language
    /// namespace: JS / TS / TSX one, C / C++ one) and that are resolved (a proven target in
    /// the index, or proven external).
    pub in_repo_resolved: usize,
    /// Call sites whose called name matches a repository declaration without a proven or
    /// inferred target.
    pub in_repo_unresolved: usize,
    /// `in_repo_resolved / (in_repo_resolved + in_repo_unresolved)`, 3 decimals; null
    /// without such call sites.
    pub in_repo_rate: Option<f64>,
    /// `server_missing` (the language has a registry backend that is not installed /
    /// available) | `server_not_ready` (the last run's readiness wait timed out,
    /// `BackendRun::ready == Some(false)`) | `server_failed` (the last run failed); null
    /// when the server ran and signalled readiness or has no backend.
    pub server: Option<&'static str>,
    /// Resolved calls whose only answer is "external" without a library location while no
    /// repository declaration carries their name (the engine's by-name rule, I-07): counted in
    /// `resolved`, reported apart so "resolved" is never mistaken for "found in a library".
    pub by_name: usize,
}

/// Cross-language bridges per kind and tier.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BridgeCount {
    /// `bridge:<kind>`.
    pub kind: &'static str,
    pub proven: usize,
    pub inferred: usize,
    pub possible: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusReport {
    pub command: &'static str,
    /// JSON schema version ([`SCHEMA`]).
    pub schema: u32,
    pub trace_version: &'static str,
    pub root: String,
    pub cache: String,
    pub index: IndexHealth,
    pub languages: Vec<LanguageRow>,
    /// One setup row per product language (server + version, toolchain, dependencies, build,
    /// ready or the one-line error; `trace_semantic::setup::report`, static checks only).
    pub setup: Vec<trace_semantic::setup::SetupRow>,
    /// Pending languages and sub-projects (not analysed yet) with their reasons.
    pub pending: Vec<PendingRow>,
    /// `allowed` (`trace index --allow-build` was given for this repository) | `not given`.
    pub build_approval: &'static str,
    /// `trace index --env <path>` remembered per ecosystem.
    pub env_paths: std::collections::BTreeMap<String, String>,
    /// The user's exclusion globs (config `inventory.exclude`); never skipped silently.
    pub excluded: Vec<String>,
    /// Settings that differ from the defaults, with their origin.
    pub settings: Vec<SettingRow>,
    /// A `trace index --watch` process keeps this repository's graph fresh now (it holds
    /// the repository's watch lock; PLAN decision 13).
    pub watching: bool,
    /// Default languages whose server installs automatically on first use.
    pub default_install: Vec<String>,
    /// Resolution health per language (call sites resolved vs unresolved).
    pub resolution: Vec<ResolutionHealth>,
    /// Library behaviour coverage of callback sites passing a function to a library callee.
    pub library_behaviour: LibraryBehaviourStatus,
    /// Bridges per kind and tier.
    pub bridges: Vec<BridgeCount>,
    /// `status --install <lang|all|default>`: what was installed (set by the CLI), else null.
    pub install: Option<crate::install::InstallReport>,
    pub backends: Vec<trace_core::model::BackendRun>,
    pub semantic: SemanticInfo,
    pub caches: CacheRates,
    pub overview: Option<Overview>,
    pub diagnostics: Vec<trace_core::model::Diagnostic>,
    pub seconds: f64,
}

/// One setting that differs from its default (`assets/config/defaults.jsonc`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SettingRow {
    /// Dotted key, e.g. `semantic.max_in_flight`.
    pub key: String,
    /// The effective value (JSON).
    pub value: String,
    /// Where it comes from: the user's `config.json` path or an environment variable name.
    pub origin: String,
}

// ---------------------------------------------------------------- index
#[derive(Clone, Debug, Default, Serialize)]
pub struct PhaseSeconds {
    pub inventory: f64,
    pub syntax: f64,
    pub semantic: f64,
    pub infer: f64,
    /// Site decisions (`trace_infer::decide`).
    pub decide: f64,
    pub persist: f64,
    /// Assembly of facts + semantics into the index (was counted inside `infer` before).
    pub assemble: f64,
    /// Override / implementation family rule (`trace_infer::family`).
    pub family: f64,
    /// Cross-language bridge detection (`trace_bridge::detect`).
    pub bridges: f64,
    /// Library knowledge phase (`trace_library`: derived summaries, standard-library index).
    pub library: f64,
}

impl PhaseSeconds {
    pub fn total(&self) -> f64 {
        self.inventory
            + self.syntax
            + self.semantic
            + self.assemble
            + self.family
            + self.library
            + self.bridges
            + self.infer
            + self.decide
            + self.persist
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct IndexReport {
    pub command: &'static str,
    /// JSON schema version ([`SCHEMA`]).
    pub schema: u32,
    pub trace_version: &'static str,
    pub root: String,
    /// `incremental` | `rebuild` (automatic, e.g. schema change) | `unchanged`.
    pub mode: &'static str,
    pub files: usize,
    pub added: usize,
    pub changed: usize,
    pub removed: usize,
    pub reparsed: usize,
    pub semantic_requeried: usize,
    /// Files of semantic partitions whose cached results were reused.
    pub semantic_reused: usize,
    /// Files not analysed yet after this run (pending languages and sub-projects).
    pub pending_files: usize,
    /// Files the servers reported as outside the build on this machine.
    pub outside_build_files: usize,
    pub symbols: usize,
    pub edges: EdgeCounts,
    pub sites: usize,
    /// Cross-language bridges detected (all tiers).
    pub bridges: usize,
    pub backends: Vec<trace_core::model::BackendRun>,
    pub languages: Vec<LanguageRow>,
    pub diagnostics: usize,
    pub seconds: PhaseSeconds,
}
