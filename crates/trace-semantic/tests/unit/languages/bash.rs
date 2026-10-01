use crate::languages::Server;
use crate::registry::{ReadySpec, Recipe, Registry, ShardMode};
use trace_core::Language;

/// The escaped source command reaches the server as ` .` / ` source` (same length); other
/// words and other languages are untouched.
#[test]
fn rule_escaped_source_command_reaches_the_server_unescaped() {
    let text =
        "#!/bin/sh\n\\. ../../nvm.sh\n\\. \"$DIR/b.sh\"\n\\source ./c.sh\necho \"a\\.b\"\nnvm use 18\n";
    let out = super::Hooks.server_text(Language::Bash, text);
    assert_eq!(out.len(), text.len());
    assert_eq!(
        out,
        "#!/bin/sh\n . ../../nvm.sh\n . \"$DIR/b.sh\"\n source ./c.sh\necho \"a\\.b\"\nnvm use 18\n"
    );
    assert!(matches!(super::Hooks.server_text(Language::Bash, "nvm ls\n"), std::borrow::Cow::Borrowed(_)));
    assert!(matches!(super::Hooks.server_text(Language::Python, "\\."), std::borrow::Cow::Borrowed(_)));
}

/// A PATH directory holding one program `mytool` that would leave a marker file if it ever
/// ran; (dir guard, marker, prepared data for that PATH).
fn machine_with_tool() -> (tempfile::TempDir, std::path::PathBuf, crate::languages::Prepared) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let marker = dir.path().join("ran");
    let platform = trace_env::os::Platform::current();
    let (name, script) = if platform.os == trace_env::os::Os::Windows {
        ("mytool.cmd", format!("@echo x > \"{}\"\r\n", marker.display()))
    } else {
        ("mytool", format!("#!/bin/sh\ntouch '{}'\n", marker.display()))
    };
    std::fs::write(bin.join(name), script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = bin.display().to_string();
    let vars =
        trace_env::os::EnvVars::from_pairs(&[("PATH", path.as_str()), ("PATHEXT", ".COM;.EXE;.BAT;.CMD")]);
    let prepared = super::bash_prepared(
        crate::languages::Prepared::default(),
        "bash-language-server test",
        &vars,
        &platform,
    );
    (dir, marker, prepared)
}

/// Rule: a command the server could not resolve that names an executable program in a PATH
/// directory of this machine is an external program; the lookup never runs it.
#[test]
fn rule_command_on_path_is_an_external_program() {
    let (_dir, marker, prepared) = machine_with_tool();
    assert!(super::Hooks.external_program("mytool", &prepared));
    assert!(super::Hooks.external_program("mytool", &prepared), "the listing is reused");
    assert!(!marker.exists(), "the program was never executed");
}

/// Rule (negative): unknown commands, paths, expansions and words that are no program
/// name stay unresolved; without the machine's data nothing is external.
#[test]
fn rule_unknown_command_stays_unresolved() {
    let (_dir, marker, prepared) = machine_with_tool();
    for word in ["nosuchtool", "./mytool", "bin/mytool", "$MYTOOL", "${CMD}", "", "-mytool"] {
        assert!(!super::Hooks.external_program(word, &prepared), "{word:?}");
    }
    assert!(!super::Hooks.external_program("mytool", &crate::languages::Prepared::default()));
    assert!(!marker.exists());
}

/// Another PATH is another answer: the program directories are part of the fingerprint.
#[test]
fn rule_program_directories_are_part_of_the_bash_fingerprint() {
    let platform = trace_env::os::Platform::current();
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    let fingerprint = |p: &std::path::Path| {
        let path = p.display().to_string();
        let vars = trace_env::os::EnvVars::from_pairs(&[("PATH", path.as_str())]);
        super::bash_prepared(crate::languages::Prepared::default(), "server 1", &vars, &platform).fingerprint
    };
    assert_eq!(fingerprint(&a), fingerprint(&a));
    assert_ne!(fingerprint(&a), fingerprint(&b));
}

#[test]
fn rule_bash_definitions_follow_sourced_files_only() {
    let registry = Registry::builtin();
    let entry = registry.entry("lsp:bash-language-server").expect("bash entry");
    assert_eq!(entry.settings["bashIde"]["includeAllWorkspaceSymbols"], false);
    assert_eq!(
        entry.ready,
        ReadySpec::Log {
            contains: "BackgroundAnalysis: Completed after".into()
        }
    );
    assert_eq!(entry.settings["bashIde"]["logLevel"], "info", "the readiness line is an info message");
    assert_eq!(entry.shard, ShardMode::Requests);
    assert_eq!(entry.runtime, vec!["node".to_string()]);
    // Default language: a complete npm closure, no licence question.
    let install = entry.install.as_ref().unwrap();
    assert!(install.licence_gate.is_none());
    let Recipe::Npm { packages } = &install.recipe else {
        panic!("bash-language-server installs from npm");
    };
    assert!(packages.iter().any(|p| p.path == "node_modules/web-tree-sitter"));
    assert!(packages.iter().all(|p| p.integrity.starts_with("sha512-")));
}
