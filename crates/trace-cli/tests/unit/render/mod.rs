use super::*;
use super::{deps::*, status::*, uses::*};
use trace_analysis::report::{
    At, BoundsInfo, BridgeInfo, CallTarget, CallerRow, CallerSite, Card, Carry, EdgeCounts, EdgeRow,
    Envelope, ImpactTotals, IndexInfo, PathRow, PhaseSeconds, ShowItem, SiteInfo, UnknownInfo,
    UnresolvedMatch,
};
use trace_analysis::report::{
    CallRow, DeepImpact, GuardTest, ImpactChain, LanguageRow, LibraryBehaviourStatus, PendingRow, Reached,
    ReferenceRow, ResolutionHealth, SettingRow, TestRow, UsesSummary,
};
use trace_core::{Language, SupportLevel};
use trace_semantic::setup::SetupRow;

fn card(id: &str, line: u32) -> Card {
    let (file, name) = id.split_once(':').unwrap();
    Card {
        id: id.into(),
        name: name.rsplit('.').next().unwrap().into(),
        qualified_name: name.into(),
        file: file.into(),
        kind: "function",
        language: Language::Python,
        line,
        end_line: line + 5,
        start_byte: 100,
        end_byte: 300,
        summary: String::new(),
        semantic: true,
    }
}

fn at(file: &str, line: u32, start_byte: u32) -> At {
    At {
        file: file.into(),
        line,
        start_byte,
        end_byte: start_byte + 4,
    }
}

fn reference(
    file: &str,
    line: u32,
    kind: &'static str,
    tier: &'static str,
    text: &str,
    via: Option<&str>,
) -> ReferenceRow {
    ReferenceRow {
        kind,
        tier,
        file: file.into(),
        language: Language::Python,
        line,
        column: 5,
        start_byte: line * 10,
        end_byte: line * 10 + 5,
        text: text.into(),
        owner: None,
        via: via.map(Into::into),
        source: "index".into(),
        resolution: "call_hierarchy",
        when: Vec::new(),
    }
}

fn unresolved(file: &str, line: u32, start_byte: u32, kind: &'static str, text: &str) -> UnresolvedMatch {
    UnresolvedMatch {
        at: at(file, line, start_byte),
        owner: None,
        kind,
        text: text.into(),
        reason: "no_semantic_target",
        rank: 0,
        scope: "name_only",
    }
}

fn completeness(status: &'static str, unresolved: Vec<UnresolvedMatch>) -> Completeness {
    Completeness {
        status,
        summary: format!("{status}: test"),
        name_matches: 10,
        resolved_to_target: 8,
        resolved_elsewhere: 0,
        unresolved,
        pending_languages: vec![],
        pending_files: 0,
        outside_build_files: 0,
        elsewhere_reasons: Default::default(),
    }
}

fn edge(
    from: &str,
    to: &str,
    kind: &'static str,
    tier: &'static str,
    file: &str,
    line: u32,
    text: &str,
) -> EdgeRow {
    EdgeRow {
        from: from.into(),
        to: to.into(),
        kind,
        tier,
        source: "gopls".into(),
        resolution: "call_hierarchy",
        at: at(file, line, line * 10),
        decision: None,
        bridge: None,
        text: text.into(),
        site: None,
    }
}

fn reached(id: &str, line: u32, tier: &'static str, distance: u32, via: &'static str, from: &str) -> Reached {
    Reached {
        card: card(id, line),
        tier,
        distance,
        via,
        from: Some(from.to_string()),
        at: Some(at(from.split_once(':').map_or(from, |(f, _)| f), line, line * 10)),
    }
}

fn bounds(complete: bool) -> BoundsInfo {
    BoundsInfo {
        hit: if complete { vec![] } else { vec!["depth"] },
        complete,
        work: 12,
        depth: 64,
    }
}

fn envelope(command: &'static str) -> Envelope {
    Envelope {
        command,
        schema: trace_analysis::report::SCHEMA,
        trace_version: "0.1.0",
        root: "C:/repo".into(),
        include: "inferred",
        index: IndexInfo {
            fresh: true,
            updated: "none",
            files: 3,
            symbols: 30,
            fingerprint: "f".into(),
        },
        seconds: 0.1,
        tiers_used: vec![],
        bridges: true,
        completeness: None,
    }
}

fn call_row(line: u32, call: &str, when: &[&str], targets: &[(&str, &str, u32)]) -> CallRow {
    CallRow {
        at: at("app.go", line, line * 10),
        kind: "call",
        tier: if targets.is_empty() { "possible" } else { "proven" },
        call: call.into(),
        when: when.iter().map(|w| w.to_string()).collect(),
        targets: targets
            .iter()
            .map(|(id, file, line)| CallTarget {
                id: id.to_string(),
                file: file.to_string(),
                line: *line,
                tier: "proven",
            })
            .collect(),
        undecided: false,
    }
}

#[test]
fn numbered_source_uses_absolute_lines_and_preserves_contents() {
    assert_eq!(
        numbered_source("def f():\r\n    return 'λ'\r\n\r\n", 62),
        "62 | def f():\n63 |     return 'λ'\n64 | "
    );
    assert_eq!(numbered_source("\tvalue = 1", 100), "100 | \tvalue = 1");
    assert_eq!(numbered_source("", 9), "");
}

#[test]
fn show_prints_a_header_and_the_verbatim_source() {
    let mut c = card("app.go:redirectFixedPath", 680);
    c.end_line = 690;
    let r = ShowReport {
        envelope: envelope("show"),
        symbols: vec![
            ShowItem {
                symbol: c,
                source: "func redirectFixedPath(c *Context) bool {\n\treturn false\n}\n".into(),
                callers: 1,
                calls: 4,
            },
            ShowItem {
                symbol: card("tree.go:node.getValue", 418),
                source: "\tfunc x() {}".into(),
                callers: 2,
                calls: 1,
            },
        ],
    };
    assert_eq!(
        show_text(&r),
        "app.go:680-690  redirectFixedPath  function \u{b7} 1 caller \u{b7} 4 calls\n\
             func redirectFixedPath(c *Context) bool {\n\treturn false\n}\n\n\
             tree.go:418-423  node.getValue  function \u{b7} 2 callers \u{b7} 1 call\n\tfunc x() {}"
    );
}

#[test]
fn uses_rows_when_impact_tests_and_check() {
    let mut rows = vec![
        reference(
            "app.go",
            625,
            "call",
            "proven",
            "\t\t\tif engine.RedirectFixedPath && redirectFixedPath(c, root, engine.RedirectFixedPath) {",
            None,
        ),
        reference(
            "app.go",
            680,
            "declaration",
            "proven",
            "func redirectFixedPath(c *Context, root *node, trailingSlash bool) bool {",
            None,
        ),
        reference("tree_test.go", 48, "call", "inferred", "\tredirectFixedPath(c, n, true)", None),
    ];
    rows[0].owner = Some("app.go:Engine.handleHTTPRequest".into());
    rows[0].when = vec!["value.handlers == nil".into(), "engine.RedirectFixedPath".into()];
    rows[2].owner = Some("tree_test.go:TestX".into());
    let summary = UsesSummary {
        impact: vec![ImpactChain {
            caller: "app.go:Engine.handleHTTPRequest".into(),
            entry_points: vec!["app.go:Engine.ServeHTTP".into(), "app.go:Engine.HandleContext".into()],
            entry_points_total: 2,
        }],
        tests: vec![
            GuardTest {
                test: "routes_test.go::TestRouteRedirectFixedPath".into(),
                file: "routes_test.go".into(),
                line: 201,
                sets: "\trouter.RedirectFixedPath = true".into(),
            },
            GuardTest {
                test: "tree_test.go::TestX".into(),
                file: "tree_test.go".into(),
                line: 40,
                sets: String::new(),
            },
        ],
        tests_total: 9,
    };
    let c = completeness(
        "partial",
        vec![
            unresolved("misc.go", 12, 120, "call", "x.redirectFixedPath()"),
            // Already printed as a row (same file, line, start): not repeated under check.
            unresolved("app.go", 625, 6250, "call", "dup"),
        ],
    );
    let text = uses_text("app.go:redirectFixedPath", &rows, Some(&c), &summary, None, false);
    let expected = "\
redirectFixedPath  2 uses \u{b7} 2 entry points \u{b7} 9 tests  2 unresolved
app.go:625  Engine.handleHTTPRequest
            if engine.RedirectFixedPath && redirectFixedPath(c, root, engine.RedirectFixedPath) {
            when  value.handlers == nil \u{b7} engine.RedirectFixedPath
tree_test.go:48 ~  TestX
                   redirectFixedPath(c, n, true)
impact  Engine.handleHTTPRequest \u{2190} Engine.ServeHTTP, Engine.HandleContext
tests   routes_test.go:201 TestRouteRedirectFixedPath   router.RedirectFixedPath = true
        tree_test.go:40 TestX
        +7 more \u{2192} --deep
check:
misc.go
  12 call ?  x.redirectFixedPath()";
    assert_eq!(text, expected);

    // --deep lists declarations too; an entry-point caller says so.
    let summary = UsesSummary {
        impact: vec![ImpactChain {
            caller: "a.py:main".into(),
            entry_points: vec!["a.py:main".into()],
            entry_points_total: 1,
        }],
        tests: vec![],
        tests_total: 0,
    };
    let text = uses_text("app.go:redirectFixedPath", &rows, None, &summary, None, true);
    assert!(text.contains("app.go:680  (def)\n"), "{text}");
    assert!(text.contains("\nimpact  main (entry point)"), "{text}");
    assert_eq!(text.lines().next().unwrap(), "redirectFixedPath  2 uses \u{b7} 1 entry point  complete");
}

#[test]
fn code_is_trimmed_and_capped() {
    assert_eq!(code("   x = 1  \t"), "x = 1");
    let long = "é".repeat(130);
    let capped = code(&long);
    assert_eq!(capped.chars().count(), CODE_CAP);
    assert!(capped.ends_with('\u{2026}'));
    assert_eq!(code(&"a".repeat(120)), "a".repeat(120));
    assert_eq!(short("src/a.py:Outer.Inner.f#3"), "Outer.Inner.f");
    assert_eq!(short("f"), "f");
    assert_eq!(marked("12", "inferred"), "12 ~");
    assert_eq!(marked("12", "proven"), "12");
}

fn caller(id: &str, distance: u32, through: Option<&str>) -> CallerRow {
    CallerRow {
        card: card(id, 10),
        tier: "proven",
        relation: "calls",
        distance,
        target: "src/flask/sansio/app.py:App.redirect".into(),
        reason: "calls".into(),
        call_site: Some(at(id.split_once(':').unwrap().0, 12, 120)),
        text: "    return redirect(x)".into(),
        via: None,
        through: through.map(Into::into),
    }
}

fn test_row(test: &str, directness: &'static str) -> TestRow {
    TestRow {
        test: test.into(),
        file: test.split_once("::").unwrap().0.into(),
        line: 3,
        mentions: "redirect".into(),
        directness,
    }
}

fn deep(transitive: Vec<CallerRow>, transitive_total: usize, tests: Vec<TestRow>) -> DeepImpact {
    DeepImpact {
        callers: vec![caller("src/flask/helpers.py:redirect", 1, None)],
        other_references: vec![],
        transitive,
        transitive_total,
        totals: ImpactTotals::default(),
        result_uses: vec![],
        similar_code: vec![],
        tests,
        unknown: UnknownInfo {
            unresolved_inside: vec![],
            undecided_sites_into_targets: 0,
            pending_languages: vec![],
            bounds: bounds(true),
        },
    }
}

#[test]
fn uses_deep_blocks() {
    let through = Some("src/flask/helpers.py:redirect");
    let impact = deep(
        vec![
            caller("examples/auth.py:login", 2, through),
            caller("examples/blog.py:create", 2, through),
            caller("examples/auth.py:logout", 2, through),
            caller("tests/test_x.py:test_y.index", 3, None),
        ],
        6,
        vec![
            test_row("tests/test_helpers.py::test_a", "direct"),
            test_row("tests/test_helpers.py::test_b", "direct"),
            test_row("tests/test_regression.py::test_c", "direct"),
            test_row("tests/test_signals.py::test_d", "via_caller"),
            test_row("tests/test_signals.py::test_e", "via_caller"),
        ],
    );
    let lines = deep_blocks(&impact);
    assert_eq!(
        lines,
        [
            "callers of callers (via redirect):",
            "  examples/auth.py: login, logout",
            "  examples/blog.py: create",
            "callers of callers:",
            "  tests/test_x.py: test_y.index",
            "  (+2 more)",
            "all tests: tests/test_helpers.py::test_a, ::test_b, tests/test_regression.py::test_c (+2 via callers)",
        ]
    );
    let rows = vec![reference(
        "src/flask/helpers.py",
        276,
        "call",
        "proven",
        "    return ctx.app.redirect(location, code=code)",
        None,
    )];
    let text = uses_text(
        "src/flask/sansio/app.py:App.redirect",
        &rows,
        None,
        &UsesSummary::default(),
        Some(&impact),
        true,
    );
    assert!(text.starts_with("App.redirect  1 use \u{b7} 6 callers of callers  complete\n"), "{text}");
    assert!(text.contains("\ncallers of callers (via redirect):\n"), "{text}");
}

#[test]
fn deep_names_and_tests_are_capped() {
    let transitive: Vec<CallerRow> = (0..10).map(|i| caller(&format!("a.py:f{i}"), 2, None)).collect();
    let tests: Vec<TestRow> = (0..12)
        .map(|i| test_row(&format!("t.py::t{i}"), "via_caller"))
        .collect();
    let lines = deep_blocks(&deep(transitive, 10, tests));
    assert_eq!(lines[0], "callers of callers:");
    assert_eq!(lines[1], "  a.py: f0, f1, f2, f3, f4, f5, f6, f7 (+2)");
    assert_eq!(
        lines[2],
        "all tests: t.py::t0, ::t1, ::t2, ::t3, ::t4, ::t5, ::t6, ::t7, ::t8, ::t9 (+2 more via callers)"
    );
    assert_eq!(lines.len(), 3);
    let direct: Vec<TestRow> = (0..11).map(|i| test_row(&format!("t.py::t{i}"), "direct")).collect();
    assert!(tests_line(&direct).unwrap().ends_with(", ::t9 (+1 more direct)"));
    assert_eq!(tests_line(&[]), None);
}

/// A router `handleHTTPRequest` example: shared conditions print once above their calls,
/// the rest follow each call, then the target; same-file targets print `:line`.
#[test]
fn deps_calls_group_shared_conditions() {
    let loop_ = "t[i].method == httpMethod";
    let calls = vec![
        call_row(
            598,
            "cleanPath(rPath)",
            &["engine.RemoveExtraSlash"],
            &[("path.go:cleanPath", "path.go", 21)],
        ),
        call_row(
            609,
            "root.getValue(rPath, c.params, c.skippedNodes, unescape)",
            &[loop_],
            &[("tree.go:node.getValue", "tree.go", 418)],
        ),
        call_row(
            616,
            "c.Next()",
            &[loop_, "value.handlers != nil"],
            &[("context.go:Context.Next", "context.go", 172)],
        ),
        call_row(
            617,
            "c.writermem.WriteHeaderNow()",
            &[loop_, "value.handlers != nil"],
            &[("response_writer.go:responseWriter.WriteHeaderNow", "response_writer.go", 69)],
        ),
        call_row(
            622,
            "redirectTrailingSlash(c)",
            &[loop_, "value.handlers == nil", "value.tsr && engine.RedirectTrailingSlash"],
            &[("app.go:redirectTrailingSlash", "app.go", 667)],
        ),
        call_row(
            625,
            "redirectFixedPath(c, root, engine.RedirectFixedPath)",
            &[loop_, "value.handlers == nil", "engine.RedirectFixedPath"],
            &[("app.go:redirectFixedPath", "app.go", 680)],
        ),
        call_row(
            639,
            "serveError(c, http.StatusMethodNotAllowed, default405Body)",
            &["engine.HandleMethodNotAllowed"],
            &[("app.go:serveError", "app.go", 650)],
        ),
        call_row(650, "mystery(c)", &[], &[]),
    ];
    let mut symbol = card("app.go:Engine.handleHTTPRequest", 588);
    symbol.end_line = 646;
    symbol.file = "app.go".into();
    let r = DependenciesReport {
        envelope: envelope("deps"),
        symbol,
        deep: false,
        calls,
        results: vec![
            reached("app.go:redirectFixedPath", 680, "proven", 1, "calls", "app.go:Engine.handleHTTPRequest"),
            reached(
                "tree.go:node.findCaseInsensitivePath",
                652,
                "proven",
                2,
                "calls",
                "app.go:redirectFixedPath",
            ),
            reached(
                "tree.go:node.findCaseInsensitivePathRec",
                689,
                "proven",
                3,
                "calls",
                "tree.go:node.findCaseInsensitivePath",
            ),
        ],
        edges: vec![
            edge(
                "app.go:Engine.handleHTTPRequest",
                "app.go:redirectFixedPath",
                "calls",
                "proven",
                "app.go",
                625,
                "x",
            ),
            edge(
                "app.go:redirectFixedPath",
                "tree.go:node.findCaseInsensitivePath",
                "calls",
                "proven",
                "app.go",
                684,
                "y",
            ),
            edge(
                "tree.go:node.findCaseInsensitivePath",
                "tree.go:node.findCaseInsensitivePathRec",
                "calls",
                "proven",
                "tree.go",
                662,
                "z",
            ),
        ],
        unresolved_inside: vec![],
        bounds: bounds(true),
        notice: "",
    };
    let row = |label: &str, left: &str, right: &str| {
        format!("  {label:>5}  {left:<58}  {right}").trim_end().to_string()
    };
    let group = |cond: &str, indent: usize| format!("  {:>5}  {}if {cond}:", "", "  ".repeat(indent));
    let expected = [
        "Engine.handleHTTPRequest  app.go:588-646 \u{b7} 8 calls \u{b7} 1 unresolved".to_string(),
        row("598", "cleanPath(rPath)", "if engine.RemoveExtraSlash  \u{2192} path.go:21"),
        group(loop_, 0),
        row("609", "  root.getValue(rPath, c.params, c.skippedNodes, unescape)", "\u{2192} tree.go:418"),
        group("value.handlers != nil", 1),
        row("616", "    c.Next()", "\u{2192} context.go:172"),
        row("617", "    c.writermem.WriteHeaderNow()", "\u{2192} response_writer.go:69"),
        group("value.handlers == nil", 1),
        row(
            "622",
            "    redirectTrailingSlash(c)",
            "if value.tsr && engine.RedirectTrailingSlash  \u{2192} :667",
        ),
        row(
            "625",
            "    redirectFixedPath(c, root, engine.RedirectFixedPath)",
            "if engine.RedirectFixedPath  \u{2192} :680",
        ),
        row(
            "639",
            "serveError(c, http.StatusMethodNotAllowed, default405Body)",
            "if engine.HandleMethodNotAllowed  \u{2192} :650",
        ),
        row("650 ?", "mystery(c)", ""),
        "then  redirectFixedPath \u{2192} node.findCaseInsensitivePath".to_string(),
        "      (+1 deeper \u{2192} --deep)".to_string(),
    ]
    .join("\n");
    assert_eq!(deps_text(&r), expected);
}

#[test]
fn path_hops_show_conditions_and_carried_values() {
    let mut hop1 = edge(
        "app.go:Engine.handleHTTPRequest",
        "app.go:redirectFixedPath",
        "calls",
        "proven",
        "app.go",
        625,
        "if engine.RedirectFixedPath && redirectFixedPath(c, root, engine.RedirectFixedPath) {",
    );
    hop1.site = Some(SiteInfo {
        call: "redirectFixedPath(c, root, engine.RedirectFixedPath)".into(),
        when: vec!["value.handlers == nil".into(), "engine.RedirectFixedPath".into()],
        carries: vec![
            Carry {
                argument: "c".into(),
                parameter: "c".into(),
            },
            Carry {
                argument: "root".into(),
                parameter: "root".into(),
            },
            Carry {
                argument: "engine.RedirectFixedPath".into(),
                parameter: "trailingSlash".into(),
            },
        ],
    });
    let mut hop2 = edge(
        "app.go:redirectFixedPath",
        "tree.go:node.findCaseInsensitivePath",
        "calls",
        "proven",
        "app.go",
        684,
        "if fixedPath, ok := root.findCaseInsensitivePath(cleanPath(rPath), trailingSlash); ok {",
    );
    hop2.site = Some(SiteInfo {
        call: "root.findCaseInsensitivePath(cleanPath(rPath), trailingSlash)".into(),
        when: vec![],
        carries: vec![
            Carry {
                argument: "cleanPath(rPath)".into(),
                parameter: "path".into(),
            },
            Carry {
                argument: "trailingSlash".into(),
                parameter: "fixTrailingSlash".into(),
            },
        ],
    });
    let mut hop3 = edge(
        "tree.go:node.findCaseInsensitivePath",
        "tree.go:node.findCaseInsensitivePathRec",
        "calls",
        "proven",
        "tree.go",
        662,
        "ciPath := n.findCaseInsensitivePathRec(",
    );
    hop3.site = Some(SiteInfo {
        call: "n.findCaseInsensitivePathRec(path, buf, [4]byte{}, fixTrailingSlash)".into(),
        when: vec![],
        carries: vec![
            Carry {
                argument: "path".into(),
                parameter: "path".into(),
            },
            Carry {
                argument: "buf".into(),
                parameter: "ciPath".into(),
            },
            Carry {
                argument: "[4]byte{}".into(),
                parameter: "rb".into(),
            },
            Carry {
                argument: "fixTrailingSlash".into(),
                parameter: "fixTrailingSlash".into(),
            },
        ],
    });
    let edges = vec![hop1, hop2, hop3];
    let mut nodes: Vec<String> = edges.iter().map(|e| e.from.clone()).collect();
    nodes.push("tree.go:node.findCaseInsensitivePathRec".into());
    let mut r = PathReport {
        envelope: envelope("path"),
        deep: false,
        from: card("app.go:Engine.handleHTTPRequest", 588),
        to: card("tree.go:node.findCaseInsensitivePathRec", 689),
        found: true,
        paths: vec![PathRow {
            nodes,
            languages: vec![Language::Go; 4],
            edges,
            tier: "proven",
        }],
        bounds: bounds(true),
        unresolved: 0,
        possible_path: false,
        note: "",
    };
    let expected = "\
Engine.handleHTTPRequest \u{2192} node.findCaseInsensitivePathRec  3 hops \u{b7} complete
app.go:625   redirectFixedPath(c, root, engine.RedirectFixedPath)
             if value.handlers == nil && engine.RedirectFixedPath
             carries  engine.RedirectFixedPath \u{2192} trailingSlash
app.go:684   root.findCaseInsensitivePath(cleanPath(rPath), trailingSlash)
             carries  cleanPath(rPath) \u{2192} path \u{b7} trailingSlash \u{2192} fixTrailingSlash
tree.go:662  n.findCaseInsensitivePathRec(path, buf, [4]byte{}, fixTrailingSlash)
             carries  path \u{2192} path \u{b7} buf \u{2192} ciPath \u{b7} [4]byte{} \u{2192} rb \u{b7} fixTrailingSlash \u{2192} fixTrailingSlash";
    assert_eq!(path_text(&r), expected);

    // A bridge hop prints the bridge it crosses.
    let mut bridge = edge(
        "web/src/api.ts:login",
        "server/app/api/auth.py:login_route",
        "bridge:http",
        "inferred",
        "web/src/api.ts",
        22,
        "  fetch(`/auth/login`, {method: \"POST\"})",
    );
    bridge.bridge = Some(BridgeInfo {
        kind: "http",
        label: "POST /auth/login".into(),
        from_language: Language::TypeScript,
        to_language: Language::Python,
        to_at: at("server/app/api/auth.py", 40, 0),
        assumptions: vec![],
        contract: None,
        candidates: 1,
    });
    r.paths[0].edges = vec![bridge];
    r.paths[0].languages = vec![Language::TypeScript, Language::Python];
    assert!(path_text(&r).ends_with(
        "web/src/api.ts:22 ~  fetch(`/auth/login`, {method: \"POST\"})   \u{21e2} http POST /auth/login \u{2192} server/app/api/auth.py:login_route"
    ));
    r.found = false;
    assert_eq!(
        path_text(&r),
        "Engine.handleHTTPRequest \u{2192} node.findCaseInsensitivePathRec  no path  complete"
    );
}

/// I-01: without a path, undecided sites that can lead to the target make the answer
/// `no path  <N> unresolved`, never `complete`; a bounded search stays `bounded`.
#[test]
fn rule_path_reports_undecided_frontier_instead_of_complete() {
    let mut r = PathReport {
        envelope: envelope("path"),
        deep: false,
        from: card("src/main.rs:main", 1),
        to: card("src/replace.rs:Replacer.clear", 40),
        found: false,
        paths: vec![],
        bounds: bounds(true),
        unresolved: 2,
        possible_path: false,
        note: "",
    };
    assert_eq!(path_text(&r), "main \u{2192} Replacer.clear  no path  2 unresolved");
    r.unresolved = 0;
    assert_eq!(path_text(&r), "main \u{2192} Replacer.clear  no path  complete");
    r.unresolved = 2;
    r.bounds = bounds(false);
    assert_eq!(path_text(&r), "main \u{2192} Replacer.clear  no path  bounded");
    // A path below the selected view (a possible cross-language link): say so.
    r.possible_path = true;
    assert_eq!(
        path_text(&r),
        "main \u{2192} Replacer.clear  no path  a possible path exists (trace path --deep)"
    );
}

/// I-01: a call whose dispatch is undecided (`self.sink.matched(..)` through a trait
/// with several implementations) counts as unresolved in the header, its row is marked
/// `?` and lists the implementations as `?` targets next to the proven declaration.
#[test]
fn rule_deps_is_not_complete_with_undecided_dispatch() {
    let mut dispatch = call_row(
        40,
        "self.sink.matched(self, &mat)",
        &[],
        &[("src/sink.rs:Sink.matched", "src/sink.rs", 10)],
    );
    dispatch.undecided = true;
    for (id, line) in [("src/json.rs:JSON.matched", 50), ("src/standard.rs:StandardSink.matched", 70)] {
        let file = id.split_once(':').unwrap().0;
        dispatch.targets.push(CallTarget {
            id: id.into(),
            file: file.into(),
            line,
            tier: "possible",
        });
    }
    let mut symbol = card("src/core.rs:Core.sink_matched", 30);
    symbol.file = "src/core.rs".into();
    symbol.end_line = 45;
    let r = DependenciesReport {
        envelope: envelope("deps"),
        symbol,
        deep: false,
        calls: vec![dispatch],
        results: vec![],
        edges: vec![],
        unresolved_inside: vec![],
        bounds: bounds(true),
        notice: "",
    };
    let text = deps_text(&r);
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("Core.sink_matched  src/core.rs:30-45 \u{b7} 1 call \u{b7} 1 unresolved"));
    let row = lines.next().unwrap();
    assert!(row.starts_with("  40 ?  self.sink.matched(self, &mat)"), "{row}");
    assert!(row.ends_with("\u{2192} src/sink.rs:10, src/json.rs:50 ?, src/standard.rs:70 ?"), "{row}");
}

/// I-14: `then` groups deeper results by their reaching parent (`Reached::from`), never
/// by the first edge that happens to reach them from another function.
#[test]
fn rule_then_groups_by_the_reaching_parent() {
    let results = vec![
        reached("a.go:b", 10, "proven", 1, "calls", "a.go:start"),
        reached("a.go:c", 20, "proven", 1, "calls", "a.go:start"),
        reached("a.go:d", 30, "proven", 2, "calls", "a.go:c"),
    ];
    assert_eq!(then_lines(&results, false), vec!["then  c \u{2192} d".to_string()]);
}

#[test]
fn context_source_callers_calls_tests_and_next() {
    let mut symbol = card("app.go:redirectFixedPath", 680);
    symbol.file = "app.go".into();
    symbol.end_line = 690;
    let r = ContextReport {
        envelope: envelope("context"),
        deep: false,
        symbol,
        source:
            "func redirectFixedPath(c *Context, root *node, trailingSlash bool) bool {\n\treturn false\n}"
                .into(),
        callers: vec![CallerSite {
            caller: "app.go:Engine.handleHTTPRequest".into(),
            at: at("app.go", 625, 6250),
            tier: "proven",
            text:
                "\t\t\tif engine.RedirectFixedPath && redirectFixedPath(c, root, engine.RedirectFixedPath) {"
                    .into(),
            when: vec!["engine.RedirectFixedPath".into()],
            carries: vec![Carry {
                argument: "engine.RedirectFixedPath".into(),
                parameter: "trailingSlash".into(),
            }],
        }],
        calls: vec![
            call_row(
                684,
                "root.findCaseInsensitivePath(cleanPath(rPath), trailingSlash)",
                &[],
                &[("tree.go:node.findCaseInsensitivePath", "tree.go", 652)],
            ),
            call_row(686, "redirectRequest(c)", &["ok"], &[("app.go:redirectRequest", "app.go", 692)]),
        ],
        callers_of_callers: vec![],
        below: vec![],
        tests: vec![GuardTest {
            test: "routes_test.go::TestRouteRedirectFixedPath".into(),
            file: "routes_test.go".into(),
            line: 201,
            sets: "router.RedirectFixedPath = true".into(),
        }],
        tests_total: 1,
        next: vec![
            "show tree.go:node.findCaseInsensitivePath".into(),
            "uses app.go:redirectFixedPath --deep".into(),
        ],
    };
    let row = |label: &str, left: &str, right: &str| {
        format!("  {label:>3}  {left:<61}  {right}").trim_end().to_string()
    };
    let expected = [
        "context redirectFixedPath  app.go:680-690 \u{b7} 1 caller \u{b7} 2 calls \u{b7} complete"
            .to_string(),
        "func redirectFixedPath(c *Context, root *node, trailingSlash bool) bool {".to_string(),
        "\treturn false".to_string(),
        "}".to_string(),
        "called by".to_string(),
        "  app.go:625  Engine.handleHTTPRequest".to_string(),
        "              if engine.RedirectFixedPath && redirectFixedPath(c, root, engine.RedirectFixedPath) {"
            .to_string(),
        "              when  engine.RedirectFixedPath".to_string(),
        "              trailingSlash \u{2190} engine.RedirectFixedPath".to_string(),
        "calls".to_string(),
        row("684", "root.findCaseInsensitivePath(cleanPath(rPath), trailingSlash)", "\u{2192} tree.go:652"),
        row("686", "redirectRequest(c)", "if ok  \u{2192} :692"),
        "tests   routes_test.go:201 TestRouteRedirectFixedPath   router.RedirectFixedPath = true".to_string(),
        "next    show tree.go:node.findCaseInsensitivePath \u{b7} uses app.go:redirectFixedPath --deep"
            .to_string(),
    ]
    .join("\n");
    assert_eq!(context_text(&r), expected);
}

fn language(language: Language, files: u32, support: SupportLevel, reason: &str) -> LanguageRow {
    LanguageRow {
        language,
        files,
        support,
        backend: None,
        backend_available: support == SupportLevel::Semantic,
        reason: reason.into(),
        resolution: None,
    }
}

fn health(
    language: Language,
    resolved: usize,
    unresolved: usize,
    warning: Option<&'static str>,
) -> ResolutionHealth {
    ResolutionHealth {
        language,
        support: SupportLevel::Semantic,
        resolved,
        unresolved,
        rate: None,
        warning,
        in_repo_resolved: resolved,
        in_repo_unresolved: unresolved,
        in_repo_rate: None,
        server: None,
        by_name: 0,
    }
}

fn setup_row(language: Language, error: Option<(&'static str, &str)>) -> SetupRow {
    SetupRow {
        language,
        backend: "b".into(),
        server: Some("Pyright 1.1.414".into()),
        toolchain: Some("python 3.12.1 (C:\\venv)".into()),
        dependencies: Some("installed (1 folder)".into()),
        build: "not needed",
        status: if error.is_some() { "error" } else { "ready" },
        error: error.map(|(_, text)| text.to_string()),
        error_type: error.map(|(kind, _)| kind),
        notes: Vec::new(),
    }
}

fn install_report() -> trace_analysis::install::InstallReport {
    trace_analysis::install::InstallReport {
        command: "status",
        trace_version: "0.1.0",
        request: "go".into(),
        languages: vec![Language::Go],
        tools_dir: "C:\\tools".into(),
        installed: vec![trace_semantic::install::InstalledTool {
            id: "gopls".into(),
            version: "v0.20.0".into(),
            dir: std::path::PathBuf::from("C:\\tools\\gopls"),
            already: false,
        }],
        already: Vec::new(),
        licence_accepted: Vec::new(),
        notes: Vec::new(),
    }
}

fn view<'a>(
    languages: &'a [LanguageRow],
    resolution: &'a [ResolutionHealth],
    setup: &'a [SetupRow],
    watching: bool,
    library: &'a LibraryBehaviourStatus,
) -> StatusView<'a> {
    StatusView {
        exists: true,
        fresh: Some(true),
        stale_files: 0,
        files: 85,
        symbols: 1724,
        pending_files: 0,
        stale: 0,
        outside_build_files: 0,
        request_failed_files: 0,
        languages,
        resolution,
        setup,
        pending: &[],
        default_install: &[],
        build_approval: "not given",
        env_paths: Vec::new(),
        excluded: &[],
        settings: &[],
        watching,
        library,
        cache: "C:\\cache\\repos\\ab12",
        install: None,
    }
}

/// `trace status` (DESIGN §4.1 task 4): one row per language with its setup (ready or the
/// one-line error), pending languages with reasons, build approval, watching.
#[test]
fn status_lines() {
    let languages = vec![
        language(Language::Python, 83, SupportLevel::Semantic, "pyright 1.1.414"),
        language(Language::Sql, 2, SupportLevel::Inventoried, "no grammar compiled in"),
        language(
            Language::Java,
            3,
            SupportLevel::Pending,
            "Java is only used in tests, fixtures or examples here",
        ),
    ];
    let resolution = vec![health(Language::Python, 260, 740, Some("low_resolution"))];
    let setup_rows = vec![
        setup_row(Language::Python, None),
        setup_row(
            Language::Scala,
            Some((
                "server_missing",
                "The Scala language server is not installed. Install it: trace status --install scala",
            )),
        ),
    ];
    let library = LibraryBehaviourStatus::default();
    let pending = vec![PendingRow {
        language: Language::Java,
        files: 3,
        reason: "Java is only used in tests, fixtures or examples here".into(),
    }];
    let install_lines = vec![
        "Python language server: installs automatically on first use (or: trace status --install default)"
            .to_string(),
    ];
    let mut v = view(&languages, &resolution, &setup_rows, false, &library);
    v.pending_files = 3;
    v.pending = &pending;
    v.default_install = &install_lines;
    let text = status_text(&v);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "index           fresh \u{b7} 85 files \u{b7} 1,724 symbols \u{b7} 26% in-repo calls resolved (26% all) \u{b7} 3 files not analyzed yet"
    );
    assert_eq!(
        lines[1],
        "python          ready \u{b7} Pyright 1.1.414 \u{b7} python 3.12.1 (C:\\venv) \u{b7} dependencies installed (1 folder) \u{b7} build not needed \u{b7} 83 files \u{b7} 26% in-repo (26% all) \u{b7} low resolution"
    );
    assert_eq!(lines[2], "sql             inventoried (no grammar compiled in) \u{b7} 2 files");
    assert_eq!(
        lines[3],
        "java            pending (Java is only used in tests, fixtures or examples here) \u{b7} 3 files"
    );
    assert_eq!(
        lines[4],
        "scala           error: The Scala language server is not installed. Install it: trace status --install scala"
    );
    assert_eq!(
        lines[5],
        "pending         Java: 3 files (Java is only used in tests, fixtures or examples here)"
    );
    assert_eq!(
        lines[6],
        "install         Python language server: installs automatically on first use (or: trace status --install default)"
    );
    assert_eq!(lines[7], "build approval  not given");
    assert_eq!(
        lines[8],
        "watching        no - run trace index --watch to keep the graph fresh while you work"
    );
    assert_eq!(lines[9], "cache           C:\\cache\\repos\\ab12");

    let mut v = view(&languages, &resolution, &setup_rows, false, &library);
    v.fresh = Some(false);
    v.stale_files = 2;
    v.stale = 3;
    v.build_approval = "allowed";
    let installed = install_report();
    v.install = Some(&installed);
    let text = status_text(&v);
    assert!(
        text.starts_with("index           stale (2 files changed) \u{2014} run: trace index \u{b7} 85 files"),
        "{text}"
    );
    assert!(text.contains("\nstale           3 files (updated before the next answer)\n"), "{text}");
    assert!(text.contains("\nbuild approval  allowed (trace index --allow-build)\n"), "{text}");
    assert!(text.ends_with("\ninstall         installed  gopls v0.20.0  C:\\tools\\gopls"), "{text}");

    let mut v = view(&languages, &resolution, &setup_rows, true, &library);
    v.exists = false;
    let text = status_text(&v);
    assert!(text.starts_with("index           not indexed \u{2014} run: trace index\n"), "{text}");
    assert!(text.contains("\nwatching        yes\n"), "{text}");
    // Non-default settings: one row each with its origin.
    let settings = vec![SettingRow {
        key: "semantic.max_in_flight".into(),
        value: "3".into(),
        origin: "/home/u/config.json".into(),
    }];
    let mut v = view(&languages, &resolution, &setup_rows, false, &library);
    v.settings = &settings;
    let text = status_text(&v);
    assert!(
        text.contains("\nsetting         semantic.max_in_flight = 3 \u{b7} /home/u/config.json\n"),
        "{text}"
    );
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_234_567), "1,234,567");
}

/// Rule 20: the index line and each language line show the in-repository rate first and
/// the raw rate in parentheses; the server state is named.
#[test]
fn rule_status_shows_in_repo_rate() {
    let languages = vec![
        language(Language::Python, 83, SupportLevel::Semantic, "pyright 1.1.414"),
        language(Language::Go, 12, SupportLevel::Semantic, "lsp:gopls v0.23.0"),
        language(Language::Rust, 4, SupportLevel::Semantic, "rust-analyzer"),
    ];
    let mut python = health(Language::Python, 260, 740, None);
    python.in_repo_resolved = 240;
    python.in_repo_unresolved = 10;
    let mut go = health(Language::Go, 0, 0, None);
    go.server = Some("server_missing");
    let mut rust = health(Language::Rust, 5, 5, Some("low_resolution"));
    rust.in_repo_resolved = 1;
    rust.in_repo_unresolved = 3;
    rust.server = Some("server_not_ready");
    let resolution = vec![python, go, rust];
    let library = LibraryBehaviourStatus::default();
    let mut v = view(&languages, &resolution, &[], false, &library);
    v.files = 99;
    let text = status_text(&v);
    let lines: Vec<&str> = text.lines().collect();
    // (240 + 1) / (250 + 4) = 95%; (260 + 5) / 1010 = 26%.
    assert_eq!(
        lines[0],
        "index           fresh \u{b7} 99 files \u{b7} 1,724 symbols \u{b7} 95% in-repo calls resolved (26% all)"
    );
    assert_eq!(
        lines[1],
        "python          semantic (pyright 1.1.414) \u{b7} 83 files \u{b7} 96% in-repo (26% all)"
    );
    assert_eq!(
        lines[2],
        "go              semantic (lsp:gopls v0.23.0) \u{b7} 12 files \u{b7} server missing"
    );
    assert_eq!(
        lines[3],
        "rust            semantic (rust-analyzer) \u{b7} 4 files \u{b7} 25% in-repo (50% all) \u{b7} low resolution \u{b7} server not ready"
    );
    // Without in-repo call sites only the raw rate is shown.
    let mut none = vec![health(Language::Python, 3, 1, None)];
    none[0].in_repo_resolved = 0;
    none[0].in_repo_unresolved = 0;
    let v = StatusView {
        resolution: &none,
        ..v
    };
    let text = status_text(&v);
    assert!(
        text.starts_with(
            "index           fresh \u{b7} 99 files \u{b7} 1,724 symbols \u{b7} 75% calls resolved\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "\npython          semantic (pyright 1.1.414) \u{b7} 83 files \u{b7} 75% calls resolved\n"
        ),
        "{text}"
    );
    // Library behaviour coverage.
    let library = LibraryBehaviourStatus {
        sites: 10,
        derived: 6,
        declared_type: 2,
        table: 1,
        coverage: Some(0.9),
    };
    let v = view(&languages, &none, &[], false, &library);
    assert!(status_text(&v).contains(
        "\nlibrary         9 of 10 functions passed to libraries have known behaviour (90%; 6 derived, 2 declared types, 1 table)\n"
    ));
}

/// Rule 3: `uses` shows at most 20 `check:` rows in the ranked report order (the file
/// line repeated whenever the file changes), then `(+N more)`.
#[test]
fn rule_check_text_is_capped_at_20_with_more_line() {
    let rows = vec![reference(
        "core/worker.py",
        3,
        "declaration",
        "proven",
        "def process(self):",
        None,
    )];
    // Ranked order: the target's file, an importer, then name-only files; files repeat.
    let mut unresolved = Vec::new();
    for i in 0..25u32 {
        let file = match i % 5 {
            0 | 1 => "core/worker.py",
            2 | 3 => "app/importer.py",
            _ => "misc/other.py",
        };
        let mut u = unresolved_entry(file, 10 + i, 100 * i, &format!("w{i}.process()"));
        u.rank = i + 1;
        unresolved.push(u);
    }
    let c = completeness("partial", unresolved);
    let text =
        uses_text("core/worker.py:Worker.process", &rows, Some(&c), &UsesSummary::default(), None, false);
    let lines: Vec<&str> = text.lines().collect();
    let check = lines.iter().position(|l| *l == "check:").expect("check: section");
    let tail = &lines[check + 1..];
    let row_lines: Vec<&&str> = tail
        .iter()
        .filter(|l| l.starts_with("  ") && !l.starts_with("  (+"))
        .collect();
    assert_eq!(row_lines.len(), CHECK_CAP);
    assert_eq!(*tail.last().unwrap(), "  (+5 more)");
    // Report order kept: worker, worker, importer, importer, other, worker, ...
    assert_eq!(
        &tail[..4],
        &[
            "core/worker.py",
            "  10 call ?  w0.process()",
            "  11 call ?  w1.process()",
            "app/importer.py"
        ]
    );
    let file_lines = tail.iter().filter(|l| !l.starts_with(' ')).count();
    assert_eq!(file_lines, 12, "{text}");
    assert!(lines[0].ends_with("  25 unresolved"), "{}", lines[0]);
}

fn unresolved_entry(file: &str, line: u32, start_byte: u32, text: &str) -> UnresolvedMatch {
    unresolved(file, line, start_byte, "call", text)
}

fn index_report() -> IndexReport {
    IndexReport {
        command: "index",
        schema: trace_analysis::report::SCHEMA,
        trace_version: "0.1.0",
        root: "C:\\repo".into(),
        mode: "incremental",
        files: 312,
        added: 1,
        changed: 2,
        removed: 0,
        reparsed: 3,
        semantic_requeried: 5,
        semantic_reused: 300,
        pending_files: 0,
        outside_build_files: 0,
        symbols: 2140,
        edges: EdgeCounts {
            proven: 5120,
            inferred: 83,
            possible: 410,
        },
        sites: 496,
        bridges: 0,
        backends: vec![],
        languages: vec![
            language(Language::Python, 300, SupportLevel::Semantic, "pyright"),
            language(
                Language::Java,
                12,
                SupportLevel::Pending,
                "Java is only used in tests, fixtures or examples here",
            ),
        ],
        diagnostics: 0,
        seconds: PhaseSeconds {
            semantic: 7.0,
            syntax: 0.9,
            ..PhaseSeconds::default()
        },
    }
}

#[test]
fn index_summary_lines() {
    let r = index_report();
    assert_eq!(
        Output::Index(r.clone()).text(),
        "indexed 312 files (+1 ~2 -0) \u{b7} 2140 symbols \u{b7} 5120 proven / 83 inferred / 410 possible \u{b7} 496 sites \u{b7} 7.9s\n  java  pending (Java is only used in tests, fixtures or examples here) \u{b7} 12 files"
    );
    assert_eq!(watch_line(&r, 0.31), "updated 3 files (+1 ~2 -0) in 0.3s");
    let json: serde_json::Value = serde_json::from_str(&Output::Index(r.clone()).json()).unwrap();
    assert_eq!(json["command"], "index");
    assert_eq!(json["edges"]["proven"], 5120);
    assert!(Output::Index(r.clone()).render(true).starts_with('{'));
    let outside = IndexReport {
        outside_build_files: 2,
        mode: "unchanged",
        languages: vec![],
        ..r
    };
    assert_eq!(
        Output::Index(outside).text(),
        "up to date: 312 files (+1 ~2 -0) \u{b7} 2140 symbols \u{b7} 5120 proven / 83 inferred / 410 possible \u{b7} 496 sites \u{b7} 7.9s\n  2 files outside the build on this computer"
    );
}
