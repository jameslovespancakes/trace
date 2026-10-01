use super::resolution::{resolved_calls, Answer};
use super::*;

/// The default `analysis.resolution_warn`.
fn warn() -> f64 {
    trace_core::config::defaults().analysis.resolution_warn
}
use std::collections::BTreeSet;
use std::collections::HashMap;
use trace_core::model::UnresolvedKind;
use trace_core::{Language, SupportLevel, SymbolId};
use trace_semantic::setup::SetupRow;

#[test]
fn rule_areas_are_two_path_segments_or_flat_modules() {
    let files = ["src/auth/session.py", "src/auth/token.py", "app.py", "src/x.py"];
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in files {
        let d = f.rsplit_once('/').map(|(d, _)| d).unwrap_or(f);
        *counts.entry(d).or_insert(0) += 1;
    }
    assert_eq!(area("src/auth/session.py", &counts, files.len()), "src/auth");
    assert_eq!(area("src/x.py", &counts, files.len()), "src");
    assert_eq!(area("app.py", &counts, files.len()), "app");
}

#[test]
fn overview_lists_entry_points_and_hubs() {
    let index = crate::test_support::project();
    let o = overview(&index, Tier::Inferred);
    let entries: Vec<&str> = o.entry_points.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(entries, vec!["api.py:login_view"]);
    let hubs: Vec<(&str, usize)> = o.hubs.iter().map(|h| (h.card.id.as_str(), h.callers)).collect();
    assert_eq!(
        hubs,
        vec![
            ("auth.py:Session._check", 1),
            ("auth.py:Session.login", 1),
            ("store.py:Store.load", 1)
        ]
    );
    let functions: usize = o.areas.iter().map(|a| a.functions).sum();
    let classes: usize = o.areas.iter().map(|a| a.classes).sum();
    assert_eq!((functions, classes), (4, 2));
}

#[test]
fn cache_rates_round_and_start_empty() {
    let mut stats = Stats::default();
    let empty = cache_rates(&stats);
    assert_eq!(empty.semantic_files.hit_rate, None);
    stats.semantic_files.record(3, 2);
    let rates = cache_rates(&stats);
    assert_eq!(rates.semantic_files.hit_rate, Some(0.667));
}

/// Rule 20: the in-repository rate counts only calls whose member names a declaration
/// of the repository (same language namespace); library calls do not dilute it. Server
/// state comes from the last run and the doctor rows.
#[test]
fn rule_status_in_repo_rate_counts_repository_names_only() {
    use trace_core::facts::{Activation, CallSite, FileFacts};
    use trace_core::model::BackendRun;
    use trace_core::ByteSpan;
    let call = |callee: &str, end: u32| CallSite {
        owner: Some(0),
        lexical_owner: Some(0),
        span: ByteSpan::new(end - callee.len() as u32, end + 2),
        callee_span: ByteSpan::new(end - callee.len() as u32, end),
        callee: callee.into(),
        member: Some(callee.rsplit('.').next().unwrap().into()),
        receiver: None,
        line: 1,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 0,
    };
    let mut index = crate::test_support::project();
    index.files[0].semantic = Some(semantics(Vec::new()));
    // api.py: `s.login` (proven edge 0 -> 2 at 20..25), `other.load` (in-repo name, no
    // target), `len` and `print` (library names; `len` resolved externally).
    let mut facts = FileFacts::default();
    facts.calls.push(call("s.login", 25));
    facts.calls.push(call("other.load", 50));
    facts.calls.push(call("len", 60));
    facts.calls.push(call("print", 70));
    index.files[0].facts = Some(facts);
    index.unresolved.push(trace_core::model::Unresolved {
        owner: Some(SymbolId(0)),
        kind: UnresolvedKind::ExternalOrAmbiguous,
        at: trace_core::Location {
            file: trace_core::FileId(0),
            bytes: ByteSpan::new(57, 60),
            line: 1,
        },
        callee: "len".into(),
        candidates: Vec::new(),
    });
    index.support.push(trace_core::LanguageSupport {
        language: Language::Python,
        files: 3,
        level: SupportLevel::Semantic,
        backend: Some("pyright".into()),
        backend_available: true,
        reason: String::new(),
    });
    let rows = resolution_health(&index, warn());
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!((r.resolved, r.unresolved), (2, 2));
    assert_eq!(r.rate, Some(0.5));
    assert_eq!((r.in_repo_resolved, r.in_repo_unresolved), (1, 1));
    assert_eq!(r.in_repo_rate, Some(0.5));
    assert_eq!(r.warning, Some("low_resolution"), "judged on the in-repo rate");
    assert_eq!(r.server, None);

    // The last run of the language's backend decides the server state.
    let run = |ok: bool, ready: Option<bool>| BackendRun {
        backend: "pyright".into(),
        languages: vec![Language::Python],
        files: 3,
        queried_files: 3,
        requests: 1,
        seconds: 0.1,
        ok,
        error: None,
        tool_version: None,
        ready,
    };
    index.backend_runs = vec![run(true, Some(false))];
    assert_eq!(resolution_health(&index, warn())[0].server, Some("server_not_ready"));
    index.backend_runs = vec![run(true, Some(false)), run(false, None)];
    assert_eq!(resolution_health(&index, warn())[0].server, Some("server_failed"));
    index.backend_runs = vec![run(true, Some(true))];
    assert_eq!(resolution_health(&index, warn())[0].server, None);

    // A language whose setup row says the server is missing is marked, with or without calls.
    index.backend_runs.clear();
    let row = |language: Language| SetupRow {
        language,
        backend: "x".into(),
        server: None,
        toolchain: None,
        dependencies: None,
        build: "not needed",
        status: "error",
        error: Some("The language server is not installed.".into()),
        error_type: Some("server_missing"),
        notes: Vec::new(),
    };
    let mut rows = resolution_health(&index, warn());
    mark_missing_servers(&mut rows, &[row(Language::Python), row(Language::Go)]);
    let states: Vec<(Language, Option<&str>)> = rows.iter().map(|r| (r.language, r.server)).collect();
    assert_eq!(
        states,
        vec![
            (Language::Python, Some("server_missing")),
            (Language::Go, Some("server_missing"))
        ]
    );
}

fn semantics(
    library_calls: Vec<trace_core::semantics::SemLibraryCall>,
) -> trace_core::semantics::FileSemantics {
    trace_core::semantics::FileSemantics {
        provider: trace_core::model::Provider::Pyright,
        tool_fingerprint: String::new(),
        edges: Vec::new(),
        unresolved: Vec::new(),
        value_refs: Vec::new(),
        diagnostics: Vec::new(),
        implementations: Vec::new(),
        resolved_elsewhere: Vec::new(),
        callback_params: Vec::new(),
        library_files: Vec::new(),
        library_calls,
        outside_build: None,
        expanded: Vec::new(),
        library_dispatch: Vec::new(),
        library_bases: Vec::new(),
    }
}

/// PLAN launch targets: a call the server resolved into installed library code counts as
/// resolved; calls of pending files and of files outside the build are not counted.
#[test]
fn rule_resolution_counts_library_calls() {
    use trace_core::facts::{Activation, CallSite, FileFacts};
    use trace_core::ByteSpan;
    let call = |callee: &str, end: u32| CallSite {
        owner: Some(0),
        lexical_owner: Some(0),
        span: ByteSpan::new(end - callee.len() as u32, end + 2),
        callee_span: ByteSpan::new(end - callee.len() as u32, end),
        callee: callee.into(),
        member: Some(callee.rsplit('.').next().unwrap().into()),
        receiver: None,
        line: 1,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 0,
    };
    let mut index = crate::test_support::project();
    let mut facts = FileFacts::default();
    facts.calls.push(call("json.loads", 40));
    facts.calls.push(call("unknown_thing", 70));
    index.files[0].facts = Some(facts.clone());
    index.files[0].semantic = Some(semantics(vec![trace_core::semantics::SemLibraryCall {
        at: ByteSpan::new(35, 40),
        line: 1,
        file: 0,
        decl_line: 10,
        decl_column: 4,
        symbol: Some("json.loads".into()),
    }]));
    let rows = resolution_health(&index, warn());
    assert_eq!((rows[0].resolved, rows[0].unresolved), (1, 1));
    // A pending file and a file outside the build add nothing.
    index.files[1].facts = Some(facts.clone());
    index.files[1].support = SupportLevel::Pending;
    index.files[2].facts = Some(facts);
    let mut outside = semantics(Vec::new());
    outside.outside_build = Some("build tag linux".into());
    index.files[2].semantic = Some(outside);
    let rows = resolution_health(&index, warn());
    assert_eq!((rows[0].resolved, rows[0].unresolved), (1, 1));
}

/// A call value flow proved to run library code (`Index::library_receivers`: a member of
/// library-created objects or of a library base) is resolved, in the rate and the dump.
#[test]
fn rule_resolution_counts_library_receivers() {
    use trace_core::facts::{Activation, CallSite, FileFacts};
    use trace_core::model::{LibraryReceiver, Location};
    use trace_core::ByteSpan;
    let call = |callee: &str, end: u32| CallSite {
        owner: Some(0),
        lexical_owner: Some(0),
        span: ByteSpan::new(end - callee.len() as u32, end + 2),
        callee_span: ByteSpan::new(end - callee.len() as u32, end),
        callee: callee.into(),
        member: Some(callee.rsplit('.').next().unwrap().into()),
        receiver: None,
        line: 1,
        activation: Activation::Plain,
        is_new: false,
        arg_count: 0,
    };
    let mut index = crate::test_support::project();
    let mut facts = FileFacts::default();
    facts.calls.push(call("client.get", 40));
    facts.calls.push(call("client.put", 70));
    index.files[0].facts = Some(facts);
    index.files[0].semantic = Some(semantics(Vec::new()));
    let rows = resolution_health(&index, warn());
    assert_eq!((rows[0].resolved, rows[0].unresolved), (0, 2));
    index.library_receivers.push(LibraryReceiver {
        at: Location {
            file: trace_core::FileId(0),
            bytes: ByteSpan::new(30, 40),
            line: 1,
        },
        library: "lib.Client.get".into(),
    });
    let rows = resolution_health(&index, warn());
    assert_eq!((rows[0].resolved, rows[0].unresolved), (1, 1));
    let dump = unresolved_dump(&index, None, 5);
    assert_eq!((dump[0].library, dump[0].unresolved), (1, 1));
}

fn call_at(callee: &str, callee_start: u32, span_end: u32) -> trace_core::facts::CallSite {
    trace_core::facts::CallSite {
        owner: Some(0),
        lexical_owner: Some(0),
        span: trace_core::ByteSpan::new(callee_start, span_end),
        callee_span: trace_core::ByteSpan::new(callee_start, callee_start + callee.len() as u32),
        callee: callee.into(),
        member: Some(callee.rsplit('.').next().unwrap().into()),
        receiver: None,
        line: 1,
        activation: trace_core::facts::Activation::Plain,
        is_new: false,
        arg_count: 0,
    }
}

/// Servers whose call ranges cover the whole invocation (`obj.method(args)` from the
/// member to the closing parenthesis) resolve the call whose callee starts or ends inside
/// the range; the call is the innermost one containing the range, so an argument call is
/// never counted by its outer call's range, and each range counts one call.
#[test]
fn rule_status_counts_edges_covering_the_callee() {
    // `w.outer(x.inner())`: outer callee 0..7, whole call 0..18; inner callee 8..15,
    // whole call 8..17. `a.b().c()`: b callee 20..23 (call 20..25), c callee 20..27 (call
    // 20..29).
    let calls = vec![
        call_at("w.outer", 0, 18),
        call_at("x.inner", 8, 17),
        call_at("a.b", 20, 25),
        call_at("a.b().c", 20, 29),
    ];
    // Whole-invocation range of `outer` only (from the member `outer` to `)`).
    let got = resolved_calls(&calls, &[(2, 18, Answer::Edge)]);
    assert_eq!(got.keys().copied().collect::<BTreeSet<_>>(), BTreeSet::from([0]));
    // The range of the inner call only: the inner call, not the outer one.
    let got = resolved_calls(&calls, &[(10, 17, Answer::Edge)]);
    assert_eq!(got.keys().copied().collect::<BTreeSet<_>>(), BTreeSet::from([1]));
    // Chained calls: `c()` (26..29) is the second step, `b()` (22..25) the first.
    let got = resolved_calls(&calls, &[(26, 29, Answer::Edge)]);
    assert_eq!(got.keys().copied().collect::<BTreeSet<_>>(), BTreeSet::from([3]));
    let got = resolved_calls(&calls, &[(22, 25, Answer::Edge)]);
    assert_eq!(got.keys().copied().collect::<BTreeSet<_>>(), BTreeSet::from([2]));
    // Callee-keyed spans (every other server) keep matching on the callee end.
    let got = resolved_calls(&calls, &[(2, 7, Answer::Edge), (10, 15, Answer::External)]);
    assert_eq!(got.get(&0), Some(&Answer::Edge));
    assert_eq!(got.get(&1), Some(&Answer::External));
    // A range outside every call resolves nothing.
    assert!(resolved_calls(&calls, &[(40, 44, Answer::Edge)]).is_empty());
    // A location beats an external-only answer for the same call.
    let got = resolved_calls(&calls, &[(2, 7, Answer::External), (2, 18, Answer::Edge)]);
    assert_eq!(got.get(&0), Some(&Answer::Edge));
}

/// Library behaviour coverage counts only callback sites whose receiving call is a
/// library call (`Site::library` set): callbacks passed to repository functions or to
/// unresolved callees are not in the denominator.
#[test]
fn rule_library_coverage_counts_library_callees_only() {
    use trace_core::model::LibraryBehaviour;
    let mut index = crate::test_support::project();
    let behaviour = |source: &str| LibraryBehaviour {
        source: source.into(),
        effect: "calls".into(),
        reason: String::new(),
        symbol: None,
        inferred: true,
    };
    let derived = crate::test_support::push_site(&mut index, "lib_derived", SiteCategory::Callback, 0, &[2]);
    index.sites[derived as usize].library = Some(behaviour("derived"));
    let none = crate::test_support::push_site(&mut index, "lib_unknown", SiteCategory::Callback, 0, &[2]);
    index.sites[none as usize].library = Some(behaviour(trace_infer::behaviour::NO_EVIDENCE));
    // Callback into a repository function / an unresolved callee: not a library site.
    crate::test_support::push_site(&mut index, "repo_callee", SiteCategory::Callback, 0, &[2]);
    crate::test_support::push_site(&mut index, "param_callee", SiteCategory::Callback, 2, &[3]);
    let s = library_behaviour(&index);
    assert_eq!(s.sites, 2);
    assert_eq!(s.derived, 1);
    assert_eq!(s.coverage, Some(0.5));
}

/// Calls answered only by the engine's by-name rule (no repository declaration carries the
/// name; recorded external without a library location) count as resolved but are reported
/// apart; library calls, repository names and local bindings are not "by name".
#[test]
fn rule_calls_resolved_by_name_are_reported_apart() {
    use trace_core::facts::FileFacts;
    use trace_core::ByteSpan;
    let mut index = crate::test_support::project();
    // api.py: `helper` (external, no library location: by name), `json.loads` (library
    // call), `load` (external, but the repository declares `load`), `cb` (local binding).
    let mut facts = FileFacts::default();
    facts.calls.push(call_at("helper", 30, 38));
    facts.calls.push(call_at("json.loads", 40, 52));
    facts.calls.push(call_at("x.load", 60, 68));
    facts.calls.push(call_at("cb", 70, 74));
    facts.local_spans.push(ByteSpan::new(70, 72));
    index.files[0].facts = Some(facts);
    index.files[0].semantic = Some(semantics(vec![trace_core::semantics::SemLibraryCall {
        at: ByteSpan::new(45, 50),
        line: 1,
        file: 0,
        decl_line: 10,
        decl_column: 4,
        symbol: Some("json.loads".into()),
    }]));
    let external = |start: u32, end: u32, callee: &str| trace_core::model::Unresolved {
        owner: Some(SymbolId(0)),
        kind: UnresolvedKind::ExternalOrAmbiguous,
        at: trace_core::Location {
            file: trace_core::FileId(0),
            bytes: ByteSpan::new(start, end),
            line: 1,
        },
        callee: callee.into(),
        candidates: Vec::new(),
    };
    index.unresolved.push(external(30, 36, "helper"));
    index.unresolved.push(external(62, 66, "x.load"));
    index.unresolved.push(external(70, 72, "cb"));
    let rows = resolution_health(&index, warn());
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].resolved, rows[0].unresolved), (4, 0));
    assert_eq!(rows[0].by_name, 1, "only `helper`");
}

/// Engine rules (I-17, I-43, I-44): a call resolved elsewhere at its member identifier (a
/// conversion to a library type, an external program) is resolved and external, never
/// "by name"; a reference listed there (the receiver module of a call) resolves no call;
/// calls in inactive preprocessor regions are outside the rate and listed in the dump.
#[test]
fn rule_calls_resolved_elsewhere_and_inactive_code_are_counted_apart() {
    use trace_core::facts::FileFacts;
    use trace_core::ByteSpan;
    let mut index = crate::test_support::project();
    let mut facts = FileFacts::default();
    facts.calls.push(call_at("curl", 30, 38)); // external program
    facts.calls.push(call_at("os.getcwd", 40, 51)); // `os` resolved elsewhere (a reference)
    facts.calls.push(call_at("legacy", 60, 68)); // inactive region
    index.files[0].facts = Some(facts);
    let mut sem = semantics(Vec::new());
    sem.resolved_elsewhere = vec![ByteSpan::new(30, 34), ByteSpan::new(40, 42)];
    index.files[0].semantic = Some(sem);
    index.unresolved.push(trace_core::model::Unresolved {
        owner: Some(SymbolId(0)),
        kind: UnresolvedKind::InactiveCode,
        at: trace_core::Location {
            file: trace_core::FileId(0),
            bytes: ByteSpan::new(60, 66),
            line: 1,
        },
        callee: "legacy".into(),
        candidates: Vec::new(),
    });
    let rows = resolution_health(&index, warn());
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].resolved, rows[0].unresolved),
        (1, 1),
        "curl resolved, os.getcwd not, legacy outside"
    );
    assert_eq!(rows[0].by_name, 0);
    let dump = unresolved_dump(&index, None, 5);
    let d = &dump[0];
    assert_eq!((d.calls, d.external, d.unresolved, d.outside_build), (2, 1, 1, 1));
    assert!(d.groups.iter().any(|g| g.reason == "inactive_code" && g.calls == 1));
}

/// I-13: the debug dump sorts every call of a language into the resolved buckets
/// (repository edge, library, by name, external, inferred) and groups the unresolved ones
/// by reason and callee (counts complete, locations cut at `top`); calls of pending files
/// are their own group, outside the analysed total.
#[test]
fn rule_unresolved_dump_groups_by_reason() {
    use trace_core::facts::FileFacts;
    use trace_core::ByteSpan;
    let mut index = crate::test_support::project();
    let mut facts = FileFacts::default();
    facts.calls.push(call_at("s.login", 18, 27)); // edge 0 -> 2 at 20..25 (callee end 25)
    facts.calls.push(call_at("json.loads", 40, 52)); // library call
    facts.calls.push(call_at("helper", 60, 68)); // by name
    facts.calls.push(call_at("x.load", 70, 78)); // ambiguous in-repo candidates
    facts.calls.push(call_at("x.load", 80, 88)); // same callee, no record
    facts.calls.push(call_at("cb.run", 90, 98)); // no record at all
    index.files[0].facts = Some(facts.clone());
    index.files[0].semantic = Some(semantics(vec![trace_core::semantics::SemLibraryCall {
        at: ByteSpan::new(45, 50),
        line: 1,
        file: 0,
        decl_line: 10,
        decl_column: 4,
        symbol: Some("json.loads".into()),
    }]));
    let record =
        |start: u32, end: u32, callee: &str, candidates: Vec<SymbolId>| trace_core::model::Unresolved {
            owner: Some(SymbolId(0)),
            kind: UnresolvedKind::ExternalOrAmbiguous,
            at: trace_core::Location {
                file: trace_core::FileId(0),
                bytes: ByteSpan::new(start, end),
                line: 1,
            },
            callee: callee.into(),
            candidates,
        };
    index.unresolved.push(record(60, 66, "helper", Vec::new()));
    index.unresolved.push(record(72, 76, "x.load", vec![SymbolId(5)]));
    // A pending file: its calls are listed, not counted as analysed.
    index.files[1].facts = Some(facts);
    index.files[1].support = SupportLevel::Pending;
    let dump = unresolved_dump(&index, Some(Language::Python), 1);
    assert_eq!(dump.len(), 1);
    let d = &dump[0];
    assert_eq!(d.calls, 6);
    assert_eq!((d.repository, d.library, d.by_name, d.external, d.inferred), (1, 1, 1, 0, 0));
    assert_eq!(d.unresolved, 3);
    assert_eq!(d.pending, 6);
    let reasons: Vec<(&str, usize)> = d.groups.iter().map(|g| (g.reason.as_str(), g.calls)).collect();
    assert_eq!(reasons, vec![("pending", 6), ("no_answer", 2), ("ambiguous", 1)]);
    let no_answer = &d.groups[1];
    assert_eq!(no_answer.callees.len(), 1, "cut at top");
    assert_eq!(no_answer.callees[0].callee, "cb.run");
    assert_eq!(no_answer.callees[0].at, vec!["api.py:1".to_string()]);
    // Totals equal the status numbers.
    let rows = resolution_health(&index, warn());
    assert_eq!(rows[0].resolved, d.repository + d.library + d.by_name + d.external + d.inferred);
    assert_eq!(rows[0].by_name, d.by_name);
    assert!(unresolved_dump(&index, Some(Language::Go), 5).is_empty());
}

/// A call whose only answer is a decided inference site (inferred edges are materialised
/// from decisions, never stored in `Index::edges`) is resolved in the status rate exactly
/// as the dump counts it `inferred`; an undecided site resolves nothing.
#[test]
fn rule_status_counts_decided_inference_sites() {
    use trace_core::facts::FileFacts;
    let mut index = crate::test_support::project();
    let mut facts = FileFacts::default();
    facts.calls.push(call_at("login", 30, 38)); // decided site at its callee
    facts.calls.push(call_at("load", 40, 48)); // undecided site at its callee
    index.files[0].facts = Some(facts);
    index.files[0].semantic = Some(semantics(Vec::new()));
    let decided = crate::test_support::push_site(&mut index, "decided", SiteCategory::NoTarget, 0, &[2]);
    let open = crate::test_support::push_site(&mut index, "open", SiteCategory::NoTarget, 0, &[5]);
    for (site, start, len) in [(decided, 30, 5), (open, 40, 4)] {
        index.sites[site as usize].at.bytes = trace_core::ByteSpan::new(start, start + len);
    }
    index.decisions[decided as usize] = crate::test_support::decided(decided, &[2]);
    let rows = resolution_health(&index, warn());
    let d = &unresolved_dump(&index, Some(Language::Python), 5)[0];
    assert_eq!((d.inferred, d.unresolved), (1, 1));
    assert_eq!((rows[0].resolved, rows[0].unresolved), (1, 1));
    assert_eq!((rows[0].in_repo_resolved, rows[0].in_repo_unresolved), (1, 1));
}

/// `trace status` lists one setup row per product language and the install line for a
/// default language whose server installs automatically; pending files are grouped.
#[test]
fn rule_status_lists_setup_rows() {
    let row = |language: Language, error_type: Option<&'static str>| SetupRow {
        language,
        backend: "b".into(),
        server: Some("server 1.0".into()),
        toolchain: None,
        dependencies: None,
        build: "not needed",
        status: if error_type.is_some() { "error" } else { "ready" },
        error: error_type.map(|_| "The language server is not installed.".to_string()),
        error_type,
        notes: Vec::new(),
    };
    let rows = [
        row(Language::Python, Some("server_missing")),
        row(Language::Scala, Some("server_missing")),
        row(Language::Go, None),
        row(Language::TypeScript, Some("server_missing")),
        row(Language::Tsx, Some("server_missing")),
    ];
    assert_eq!(
        default_install_lines(&rows),
        vec![
            "Python language server: installs automatically on first use (or: trace status --install default)".to_string(),
            "TypeScript language server: installs automatically on first use (or: trace status --install default)".to_string(),
        ]
    );
    let mut index = crate::test_support::project();
    index.files[1].support = SupportLevel::Pending;
    index.files[1].pending = Some("Python is only used in tests, fixtures or examples here".into());
    let pending = pending_rows(&index);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].files, 1);
}

#[test]
fn flat_packages_split_per_module() {
    let files: Vec<String> = (0..10).map(|i| format!("pkg/m{i}.py")).collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in &files {
        *counts.entry("pkg").or_insert(0) += f.len().min(1);
    }
    assert_eq!(area("pkg/m3.py", &counts, files.len()), "pkg/m3");
}
