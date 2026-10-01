use super::*;
use crate::os::{Arch, EnvVars};
use crate::test_support::write;

fn windows() -> Platform {
    Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    }
}

fn fake_home(dir: &Path, version: &str) -> PathBuf {
    let home = dir.join(format!("R-{version}"));
    write(&home, "library/base/DESCRIPTION", &format!("Package: base\nVersion: {version}\nPriority: base\n"));
    write(&home, "bin/x64/Rscript.exe", "");
    write(&home, "bin/Rscript", "");
    home
}

fn installed(lib: &Path, name: &str, version: &str) {
    write(lib, &format!("{name}/DESCRIPTION"), &format!("Package: {name}\nVersion: {version}\n"));
    fs::create_dir_all(lib.join(name).join("Meta")).unwrap();
}

#[test]
fn rule_dcf_fields_and_continuations() {
    let f = parse_dcf(
        "Package: dplyr\nImports: cli (>= 3.4.0),\n    generics,\n\tglue (>= 1.3.2)\nDepends: R (>= 4.1.0)\n",
    );
    let deps = description_dependencies(&f);
    let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["cli", "generics", "glue"]);
    assert_eq!(deps[0].op.as_deref(), Some(">="));
    assert_eq!(deps[0].version.as_deref(), Some("3.4.0"));
    let req = description_r_requirement(&f).unwrap();
    assert!(req.matches(&Version::parse("4.6.1").unwrap()));
    assert!(!req.matches(&Version::parse("4.0.5").unwrap()));
}

#[test]
fn rule_r_description_imports_checked_in_libraries() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pkg");
    write(&root, "DESCRIPTION", "Package: pkg\nVersion: 1.0\nDepends: R (>= 4.1), methods\nImports: cli (>= 3.4.0), glue, rlang\nSuggests: testthat\n");
    let home = fake_home(dir.path(), "4.6.1");
    let lib = dir.path().join("lib");
    installed(&lib, "cli", "3.6.5");
    installed(&lib, "glue", "1.8.0");
    installed(&lib, "rlang", "1.0.0");
    let libs = lib.display().to_string();
    let vars = EnvVars::from_pairs(&[("R_LIBS_USER", libs.as_str())]);
    let p = windows();
    let files: Vec<(&str, Language)> = Vec::new();
    let cx = DetectContext {
        root: &root,
        platform: &p,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &files,
    };
    let tc = toolchain_of(home.clone(), Origin::Override, &p);
    let s = setup(&cx, Some(&tc));
    assert_eq!(s.deps.status, DepsStatus::Installed, "{:?}", s.deps.missing);
    // An old cli and a missing rlang: both named in the hint; Suggests never required.
    let lib2 = dir.path().join("lib2");
    installed(&lib2, "cli", "3.0.0");
    installed(&lib2, "glue", "1.8.0");
    let libs2 = lib2.display().to_string();
    let vars2 = EnvVars::from_pairs(&[("R_LIBS_USER", libs2.as_str())]);
    let cx2 = DetectContext { vars: &vars2, ..cx };
    let s = setup(&cx2, Some(&tc));
    assert_eq!(s.deps.status, DepsStatus::Missing);
    assert_eq!(s.deps.missing, vec!["cli".to_string(), "rlang".to_string()]);
    assert_eq!(s.deps.hint, "install.packages(c(\"cli\", \"rlang\"))");
    assert!(s.deps.roots.iter().any(|r| r.kind == LibraryKind::Stdlib));
}

#[test]
fn rule_renv_library_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(
        &root,
        "renv.lock",
        r#"{"R": {"Version": "4.6.1"}, "Packages": {"cli": {"Package": "cli", "Version": "3.6.5"}}}"#,
    );
    let lib = root.join("renv/library/windows/R-4.6/x86_64-w64-mingw32");
    installed(&lib, "cli", "3.6.5");
    // A user library with the same package must not count: renv isolates.
    let home = fake_home(dir.path(), "4.6.1");
    let p = windows();
    let vars = EnvVars::default();
    let files: Vec<(&str, Language)> = Vec::new();
    let cx = DetectContext {
        root: &root,
        platform: &p,
        vars: &vars,
        env_override: None,
        forbidden: &[],
        files: &files,
    };
    let tc = toolchain_of(home, Origin::Override, &p);
    let s = setup(&cx, Some(&tc));
    assert_eq!(s.renv_library.as_deref(), Some(lib.as_path()));
    assert_eq!(s.libraries, vec![lib.clone()]);
    assert_eq!(s.deps.status, DepsStatus::Installed);
    fs::remove_dir_all(lib.join("cli")).unwrap();
    let s = setup(&cx, Some(&tc));
    assert_eq!(s.deps.status, DepsStatus::Missing);
    assert_eq!(s.deps.hint, RENV_HINT);
}

#[test]
fn rule_r_home_version_and_description_requirement() {
    let dir = tempfile::tempdir().unwrap();
    let old = fake_home(dir.path(), "4.0.5");
    let root = dir.path().join("pkg");
    write(&root, "DESCRIPTION", "Package: pkg\nDepends: R (>= 4.1.0)\n");
    let p = windows();
    let vars = EnvVars::default();
    let files: Vec<(&str, Language)> = Vec::new();
    let cx = DetectContext {
        root: &root,
        platform: &p,
        vars: &vars,
        env_override: Some(&old),
        forbidden: &[],
        files: &files,
    };
    // The override wins even when it is too old: the requirement is reported.
    match toolchain(&cx) {
        ToolchainStatus::TooOld {
            found,
            needed,
            source,
        } => {
            assert_eq!(found.version.unwrap().text, "4.0.5");
            assert_eq!(needed.describe("R"), "R 4.1.0 or newer");
            assert_eq!(source, "DESCRIPTION");
        }
        other => panic!("{other:?}"),
    }
    let new = fake_home(dir.path(), "4.6.1");
    let cx = DetectContext {
        env_override: Some(&new),
        ..cx
    };
    match toolchain(&cx) {
        ToolchainStatus::Found(t) => {
            assert_eq!(t.facts.get("r_minor").map(String::as_str), Some("4.6"));
            assert!(t.executables.contains_key("Rscript"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        rversion_header(b"#define R_MAJOR  \"4\"\n#define R_MINOR  \"6.1\"\n").map(|v| v.text),
        Some("4.6.1".to_string())
    );
}

#[test]
fn rule_namespace_directives_from_the_syntax_tree() {
    let ns = read_namespace(b"export(summarise)\nexportPattern(\"^[^\\\\.]\")\nimport(rlang)\nimportFrom(glue, glue, glue_collapse)\nif (getRversion() >= \"4.0\") importFrom(utils, head)\n");
    assert_eq!(ns.exports, vec!["summarise".to_string()]);
    assert!(ns.export_pattern);
    assert_eq!(ns.imports, vec!["rlang".to_string()]);
    assert!(ns
        .import_from
        .contains(&("glue".to_string(), "glue_collapse".to_string())));
    assert!(ns.import_from.contains(&("utils".to_string(), "head".to_string())));
    let q = qualified_symbols(b"f <- function(x) rlang::abort(x)\ng <- function() dplyr:::helper()\n");
    assert_eq!(
        q,
        vec![
            ("rlang".to_string(), "abort".to_string()),
            ("dplyr".to_string(), "helper".to_string())
        ]
    );
    let a = attached_packages(b"library(dplyr)\nrequire(\"tidyr\")\nlibrary(pkg, character.only = TRUE)\n");
    assert_eq!(a, vec!["dplyr".to_string(), "tidyr".to_string()]);
}

#[test]
fn rule_r_env_path_is_a_library() {
    let dir = tempfile::tempdir().unwrap();
    installed(dir.path(), "cli", "3.6.5");
    assert!(accepts_env_path(dir.path()));
    assert!(!accepts_env_path(&dir.path().join("cli")));
    assert_eq!(install_hint(&["a".into()]), "install.packages(\"a\")");
    let many: Vec<String> = (1..=7).map(|i| format!("p{i}")).collect();
    assert_eq!(install_hint(&many), "install.packages(c(\"p1\", \"p2\", \"p3\", \"p4\", \"p5\")) and 2 more");
}
