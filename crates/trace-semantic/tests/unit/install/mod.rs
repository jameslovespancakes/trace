use super::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-install-{name}-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A zip with `bin/<exe>` (the tool) and a README.
fn tool_zip(platform: &Platform) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
    w.start_file(format!("x-1.0/{}", archive::single_file_name("bin/x", platform)), opts)
        .unwrap();
    w.write_all(b"binary").unwrap();
    w.start_file("x-1.0/README", opts).unwrap();
    w.write_all(b"readme").unwrap();
    w.finish().unwrap().into_inner()
}

/// A test registry: `language` served by `lsp:test-<id>` with an archive record whose
/// pinned download is already in `<tools>/downloads` (so nothing touches the network).
fn registry_with(tools: &Path, entries: &[(Language, &str, bool)], platform: &Platform) -> Registry {
    let bytes = tool_zip(platform);
    let sha = fetch::sha256_hex(&bytes);
    fs::create_dir_all(tools.join("downloads")).unwrap();
    let mut backends = Vec::new();
    for (language, id, gated) in entries {
        // Every record gets its own cached copy (installs delete their downloads).
        fs::write(tools.join("downloads").join(&sha), &bytes).unwrap();
        let mut install = serde_json::json!({
            "id": id, "version": "1.0", "license": "MIT", "display": format!("{} language server", language.display_name()),
            "product": "X", "recipe": "archive",
            "artifacts": [{"platform": "any", "url": format!("https://example.invalid/{id}.zip"), "sha256": sha, "strip": 1}],
            "executables": ["bin/x"]
        });
        if *gated {
            install["licence_gate"] =
                serde_json::json!({"url": "https://example.invalid/licence", "summary": "Read it."});
        }
        backends.push(
            serde_json::from_value::<BackendEntry>(serde_json::json!({
                "id": format!("lsp:test-{id}"),
                "kind": "lsp",
                "languages": [language],
                "language_ids": ["x"],
                "server": {"name": id, "version": "1.0", "license": "MIT"},
                "executable": {"from": "tool", "tool": id, "path": "bin/x"},
                "install": install,
                "safety": "test"
            }))
            .unwrap(),
        );
    }
    let registry = Registry {
        backends,
        runtimes: Vec::new(),
    };
    registry.validate().unwrap();
    registry
}

fn request<'a>(
    tools: &'a Path,
    registry: &'a Registry,
    languages: &'a [Language],
    platform: &'a Platform,
    vars: &'a EnvVars,
    progress: &'a mut dyn FnMut(&InstallProgress),
    licences: LicenceAnswer<'a>,
) -> InstallRequest<'a> {
    InstallRequest {
        tools_dir: tools,
        registry,
        languages,
        repo_root: None,
        platform,
        vars,
        progress,
        licences,
    }
}

/// A second install of the same pinned version is a no-op (MANIFEST + directory), the
/// first one unpacks, records exec hashes and deletes its download.
#[test]
fn rule_installer_is_idempotent() {
    let tools = temp("idempotent");
    let platform = Platform::current();
    let registry = registry_with(&tools, &[(Language::Go, "xls", false)], &platform);
    let vars = EnvVars::default();
    let mut lines = Vec::new();
    let mut progress = |p: &InstallProgress| lines.push((p.step, progress_line(p)));
    let first = install(request(
        &tools,
        &registry,
        &[Language::Go],
        &platform,
        &vars,
        &mut progress,
        LicenceAnswer::Refuse,
    ))
    .unwrap();
    assert_eq!(first.len(), 1);
    assert!(!first[0].already);
    let dir = tools.join("xls").join("1.0");
    assert_eq!(first[0].dir, dir);
    assert!(archive::executable_path(&dir, "bin/x", &platform).is_some());
    assert!(dir.join("README").is_file());
    let m = Manifest::load(&tools);
    assert_eq!(m.tools["xls"].method, "archive");
    assert_eq!(m.tools["xls"].files.len(), 1);
    assert_eq!(lines.iter().filter(|(s, _)| *s == "start").count(), 1);
    assert!(lines
        .iter()
        .any(|(_, l)| l == "Installing the Go language server (X 1.0)..."));
    assert!(fs::read_dir(tools.join("downloads")).unwrap().next().is_none(), "download removed");
    let mut progress = |_: &InstallProgress| panic!("nothing to do the second time");
    let second = install(request(
        &tools,
        &registry,
        &[Language::Go],
        &platform,
        &vars,
        &mut progress,
        LicenceAnswer::Refuse,
    ))
    .unwrap();
    assert!(second[0].already);
    assert!(!tools
        .read_dir()
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(".staging-")));
    let _ = fs::remove_dir_all(&tools);
}

/// PLAN decision 11: a gated tool is refused without `--yes` when nobody can be asked
/// (nothing downloaded or unpacked), refused when the user answers no, installed when the
/// user accepts; the acceptance is remembered.
#[test]
fn rule_licence_gate_refuses_without_yes_when_not_interactive() {
    let tools = temp("licence");
    let platform = Platform::current();
    let registry = registry_with(&tools, &[(Language::Php, "gated", true)], &platform);
    let vars = EnvVars::default();
    let mut progress = |_: &InstallProgress| {};
    let err = install(request(
        &tools,
        &registry,
        &[Language::Php],
        &platform,
        &vars,
        &mut progress,
        LicenceAnswer::Refuse,
    ))
    .unwrap_err();
    assert_eq!(err.kind(), "install_failed");
    let text = err.lines().join("\n");
    assert!(text.starts_with("The PHP language server is installed only after you accept its licence (https://example.invalid/licence)."), "{text}");
    assert!(text.contains("trace status --install php --yes"), "{text}");
    assert!(!tools.join("gated").exists());
    let mut asked = 0;
    let mut no = |_: &InstallSpec, _: &LicenceGate| {
        asked += 1;
        false
    };
    let mut progress = |_: &InstallProgress| {};
    let err = install(request(
        &tools,
        &registry,
        &[Language::Php],
        &platform,
        &vars,
        &mut progress,
        LicenceAnswer::Ask(&mut no),
    ))
    .unwrap_err();
    assert_eq!(err.kind(), "install_failed");
    assert_eq!(asked, 1);
    let mut yes = |_: &InstallSpec, gate: &LicenceGate| gate.url.starts_with("https://");
    let mut progress = |_: &InstallProgress| {};
    let ok = install(request(
        &tools,
        &registry,
        &[Language::Php],
        &platform,
        &vars,
        &mut progress,
        LicenceAnswer::Ask(&mut yes),
    ))
    .unwrap();
    assert!(!ok[0].already);
    let m = Manifest::load(&tools);
    assert!(m.licence_accepted("gated", "1.0"));
    assert!(m.tools["gated"].licence_accepted);
    // Installed and accepted: a later non-interactive run is fine.
    let mut progress = |_: &InstallProgress| {};
    assert!(install(request(
        &tools,
        &registry,
        &[Language::Php],
        &platform,
        &vars,
        &mut progress,
        LicenceAnswer::Refuse
    ))
    .is_ok());
    let _ = fs::remove_dir_all(&tools);
}

/// PLAN decision 10: automatic installs cover only default languages and never a gated
/// tool; installed defaults are not missing.
#[test]
fn rule_auto_install_never_installs_non_default_or_gated_tools() {
    let tools = temp("auto");
    let platform = Platform::current();
    let registry = registry_with(
        &tools,
        &[
            (Language::Scala, "kls", false),
            (Language::Python, "gpy", true),
            (Language::Go, "gls", false),
        ],
        &platform,
    );
    let all = [Language::Scala, Language::Python, Language::Go];
    assert_eq!(missing_defaults(&tools, &registry, &all, &platform), vec![Language::Go]);
    let vars = EnvVars::default();
    let mut progress = |_: &InstallProgress| {};
    let mut yes = |_: &InstallSpec, _: &LicenceGate| true;
    // Even an accepting answer is ignored: automatic installs refuse every licence.
    let done = auto_install(request(
        &tools,
        &registry,
        &all,
        &platform,
        &vars,
        &mut progress,
        LicenceAnswer::Ask(&mut yes),
    ))
    .unwrap();
    assert_eq!(done.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["gls"]);
    assert!(!tools.join("kls").exists() && !tools.join("gpy").exists());
    assert!(missing_defaults(&tools, &registry, &all, &platform).is_empty());
    let mut progress = |_: &InstallProgress| {};
    let none = auto_install(request(
        &tools,
        &registry,
        &all,
        &platform,
        &vars,
        &mut progress,
        LicenceAnswer::Refuse,
    ))
    .unwrap();
    assert!(none.is_empty());
    let _ = fs::remove_dir_all(&tools);
}

/// One line per tool when stderr is not a terminal; rewritten in place with the
/// percentage and ` done` on a terminal.
#[test]
fn rule_install_progress_is_one_line_per_tool() {
    let ev = |step: &'static str, done: u64| InstallProgress {
        what: "Python language server".into(),
        product: "Pyright".into(),
        version: "1.1.414".into(),
        step,
        done,
        total: Some(200),
    };
    let events = [
        ev("start", 0),
        ev("download", 100),
        ev("download", 200),
        ev("extract", 0),
        ev("done", 0),
    ];
    let mut out = Vec::new();
    let mut plain = ProgressPrinter::new(false);
    for e in &events {
        plain.write(&mut out, e).unwrap();
    }
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Installing the Python language server (Pyright 1.1.414)...\n"
    );
    let mut out = Vec::new();
    let mut tty = ProgressPrinter::new(true);
    for e in &events {
        tty.write(&mut out, e).unwrap();
    }
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains(" 50%") && text.contains(" 100%"), "{text}");
    assert!(text.ends_with("(Pyright 1.1.414)... done    \n"), "{text}");
    assert_eq!(text.matches('\n').count(), 1);
}
