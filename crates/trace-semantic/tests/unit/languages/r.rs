use super::*;
use crate::registry::Registry;
use trace_env::os::{Arch, EnvVars, Os, Platform};

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-r-{name}-{}", uuid::Uuid::new_v4().simple()))
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn installed(lib: &Path, name: &str, namespace: &str) {
    write(lib, &format!("{name}/DESCRIPTION"), &format!("Package: {name}\nVersion: 1.2.3\n"));
    write(lib, &format!("{name}/NAMESPACE"), namespace);
    fs::create_dir_all(lib.join(name).join("Meta")).unwrap();
}

#[test]
fn rule_r_needs_build_approval() {
    let dir = temp("approval");
    let root = dir.join("repo");
    write(&root, "R/a.R", "f <- function(x) x\n");
    write(&root, "DESCRIPTION", "Package: a\nImports: cli\n");
    let repo = trace_core::paths::RepoPaths::resolve_in(&root, &dir.join("home")).unwrap();
    let registry = Registry::builtin();
    let entry = registry.entry("lsp:r-languageserver").unwrap();
    let tools = crate::test_support::setup::tool_env(None);
    let settings = trace_core::repo_settings::RepoSettings::default();
    let files = [("R/a.R", Language::R)];
    let facts = |_: &str| -> Option<&FileFacts> { None };
    let platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    let vars = EnvVars::default();
    let cx = SetupContext {
        repo: &repo,
        entry,
        languages: &[Language::R],
        files: &files,
        facts: &facts,
        settings: &settings,
        tools: &tools,
        platform: &platform,
        vars: &vars,
        report_only: true,
    };
    let text = Hooks.preflight(&cx).unwrap_err().lines().join("\n");
    assert!(text.contains("R needs an R session, which runs the code of this project's packages."), "{text}");
    assert!(text.contains("R is not installed. Install it from https://cloud.r-project.org"), "{text}");
    assert!(text.contains("The R language server is not installed."), "{text}");
    // With approval only the missing R and server remain.
    let allowed = trace_core::repo_settings::RepoSettings {
        allow_build: true,
        ..Default::default()
    };
    let cx = SetupContext {
        settings: &allowed,
        ..cx
    };
    let text = Hooks.preflight(&cx).unwrap_err().lines().join("\n");
    assert!(!text.contains("--allow-build"), "{text}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rule_r_binary_snapshot_closure_is_pinned_per_platform() {
    let registry = Registry::builtin();
    let entry = registry.entry("lsp:r-languageserver").unwrap();
    let Recipe::RPackage {
        package,
        snapshot_date,
        files,
    } = &entry.install.as_ref().unwrap().recipe
    else {
        panic!("r_package recipe");
    };
    assert_eq!(package, "languageserver");
    assert!(snapshot_date.len() == 10 && snapshot_date.starts_with("20"), "{snapshot_date}");
    // Every (platform, distro, R minor) target lists the same closure, ending with the
    // server, every file with an https url on the dated snapshot and a sha256.
    let mut targets: BTreeMap<(String, Option<String>, String), Vec<&str>> = BTreeMap::new();
    for f in files {
        assert!(f.url.starts_with("https://packagemanager.posit.co/cran/"), "{}", f.url);
        assert!(f.url.contains(snapshot_date.as_str()), "{}", f.url);
        assert!(f.sha256.len() == 64 && f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(f.distro.is_some(), f.platform.starts_with("linux-"));
        targets
            .entry((f.platform.clone(), f.distro.clone(), f.r_minor.clone()))
            .or_default()
            .push(f.package.as_str());
    }
    let mut closure: Option<BTreeSet<&str>> = None;
    for (target, packages) in &targets {
        assert_eq!(packages.last(), Some(&"languageserver"), "{target:?}");
        let set: BTreeSet<&str> = packages.iter().copied().collect();
        assert_eq!(set.len(), packages.len(), "{target:?} lists a package twice");
        match &closure {
            None => closure = Some(set),
            Some(c) => assert_eq!(c, &set, "{target:?}"),
        }
    }
    for key in ["windows-x86_64", "macos-aarch64", "macos-x86_64", "linux-x86_64"] {
        for minor in ["4.5", "4.6"] {
            assert!(
                targets.keys().any(|(p, _, m)| p == key && m == minor),
                "no binaries for {key} R {minor}"
            );
        }
    }
    // An R minor without binaries is not installable here: the advice names the pinned ones.
    let linux = Platform {
        os: Os::Linux,
        arch: Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    assert!(r_binaries_for(files, &linux, Some("noble"), "4.6").len() > 1);
    assert!(r_binaries_for(files, &linux, Some("noble"), "4.2").is_empty());
    assert_eq!(r_minors_for(files, &linux, Some("noble")), vec!["4.5".to_string(), "4.6".to_string()]);
}

#[test]
fn rule_r_deparsed_temp_target_maps_to_package_symbol() {
    let dir = temp("deparsed");
    let root = dir.join("repo");
    write(&root, "NAMESPACE", "import(rlang)\nimportFrom(glue, glue)\n");
    write(&root, "R/a.R", "f <- function(x) { cli::cli_abort(x); abort(x); glue(x); filter(x) }\n");
    write(&root, "R/b.R", "library(dplyr)\ng <- function(x) stats::filter(x)\n");
    let lib = dir.join("lib");
    installed(&lib, "rlang", "export(abort)\nexport(enquo)\n");
    installed(&lib, "glue", "export(glue)\n");
    installed(&lib, "cli", "export(cli_abort)\n");
    installed(&lib, "dplyr", "export(filter)\n");
    let system = dir.join("R/library");
    installed(&system, "stats", "export(filter)\nexport(median)\n");
    let data = RData::new(
        vec![root.join("R/a.R"), root.join("R/b.R")],
        Some(root.join("NAMESPACE")),
        vec![lib.clone(), system.clone()],
        Some(system.clone()),
    );
    let tmp = dir.join("state/tmp/RtmpAbc123");
    let deparsed = |sym: &str| -> PathBuf {
        let p = tmp.join(format!("{sym}.R"));
        write(&tmp, &format!("{sym}.R"), &format!("{DEPARSED_HEADER}\n{sym} <- function(...) NULL\n"));
        p
    };
    let loc = r_location(&deparsed("abort"), &data).unwrap();
    assert_eq!(loc.symbol.as_deref(), Some("rlang::abort"));
    assert_eq!((loc.package.as_str(), loc.version.as_deref()), ("rlang", Some("1.2.3")));
    assert!(loc.readable && !loc.stdlib);
    assert_eq!(r_location(&deparsed("glue"), &data).unwrap().symbol.as_deref(), Some("glue::glue"));
    assert_eq!(r_location(&deparsed("cli_abort"), &data).unwrap().symbol.as_deref(), Some("cli::cli_abort"));
    let median = r_location(&deparsed("median"), &data).unwrap();
    assert_eq!(median.symbol.as_deref(), Some("stats::median"));
    assert!(median.stdlib);
    // `filter`: dplyr (attached) and stats (qualified) both supply it -> not named.
    assert!(r_location(&deparsed("filter"), &data).is_none());
    // Not a deparsed file (no header) and unknown symbols stay unnamed.
    write(&tmp, "plain.R", "x <- 1\n");
    assert!(r_location(&tmp.join("plain.R"), &data).is_none());
    assert!(r_location(&deparsed("nothing_here"), &data).is_none());
    // A file inside an installed package names the package directly.
    let inside = r_location(&lib.join("rlang/R/rlang"), &data).unwrap();
    assert_eq!(inside.package, "rlang");
    assert!(inside.symbol.is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn rule_symbol_poll_query_is_a_declared_name() {
    let names = vec![
        ("R/a.R".to_string(), vec!["summarise_at".to_string(), "mutate".to_string()]),
        ("R/b.R".to_string(), vec!["summarise_if".to_string()]),
        ("R/c.R".to_string(), vec!["sum_up".to_string(), "mutate_all".to_string()]),
    ];
    // "sum" is a prefix of declared names in all three files.
    let q = symbol_poll_query(&names).unwrap();
    assert_eq!(q, "sum");
    assert!(names.iter().flat_map(|(_, n)| n).any(|n| n.starts_with(&q)));
    assert_eq!(symbol_poll_query(&[("R/x.R".into(), vec!["f".into()])]).as_deref(), Some("f"));
    assert_eq!(symbol_poll_query(&[]), None);
}

#[test]
fn rule_r_missing_package_in_server_log_is_named() {
    assert_eq!(
        missing_package("Error in loadNamespace(x) : there is no package called \u{2018}cli\u{2019}"),
        Some("cli".to_string())
    );
    assert_eq!(missing_package("there is no package called 'x'"), Some("x".to_string()));
    assert_eq!(missing_package("all good"), None);
}
