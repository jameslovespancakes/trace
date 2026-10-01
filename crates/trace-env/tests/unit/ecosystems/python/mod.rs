use super::*;
use crate::os::Arch;
use crate::test_support::write;

fn platform(os: Os) -> Platform {
    Platform {
        os,
        arch: Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    }
}

fn cx<'a>(
    root: &'a Path,
    platform: &'a Platform,
    vars: &'a EnvVars,
    env: Option<&'a Path>,
) -> DetectContext<'a> {
    DetectContext {
        root,
        platform,
        vars,
        env_override: env,
        forbidden: &[],
        files: &[],
    }
}

fn venv(root: &Path, rel: &str, version: &str, packages: &[&str]) {
    write(root, &format!("{rel}/pyvenv.cfg"), &format!("home = /nowhere\nversion_info = {version}\n"));
    for p in packages {
        fs::create_dir_all(root.join(rel).join("Lib/site-packages").join(p)).unwrap();
    }
    fs::create_dir_all(root.join(rel).join("Lib/site-packages")).unwrap();
}

#[test]
fn rule_env_finds_a_venv_and_never_a_dotenv_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, ".env", "SECRET=1\n");
    write(root, "backend/.venv/pyvenv.cfg", "home = x\nversion_info = 3.12.4\n");
    fs::create_dir_all(root.join("backend/.venv/Lib/site-packages/requests")).unwrap();
    let p = platform(Os::Windows);
    let vars = EnvVars::default();
    let py = find_environment(&cx(root, &p, &vars, None))
        .unwrap()
        .expect("venv one level below the root");
    assert!(py.root.ends_with(".venv"));
    assert_eq!(py.version.as_deref(), Some("3.12"));
    assert_eq!(py.origin, Origin::Project);
}

#[test]
fn rule_venv_found_in_project() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    venv(root, ".venv", "3.13", &[]);
    let p = platform(Os::Windows);
    let vars = EnvVars::default();
    let found = find_environment(&cx(root, &p, &vars, None)).unwrap().unwrap();
    assert!(found.root.ends_with(".venv"));
    assert_eq!(found.kind, EnvKind::Venv);
    assert_eq!(found.version.as_deref(), Some("3.13"));
    // An explicit override that is not an environment is an error, never a fallback.
    let nothing = root.join("nothing");
    fs::create_dir_all(&nothing).unwrap();
    assert_eq!(find_environment(&cx(root, &p, &vars, Some(&nothing))), Err(nothing.clone()));
    // uv's UV_PROJECT_ENVIRONMENT (relative to the project) wins over .venv.
    venv(root, "custom-env", "3.11", &[]);
    let vars = EnvVars::from_pairs(&[("UV_PROJECT_ENVIRONMENT", "custom-env")]);
    let found = find_environment(&cx(root, &p, &vars, None)).unwrap().unwrap();
    assert!(found.root.ends_with("custom-env"));
}

#[test]
fn rule_conda_env_without_pyvenv_cfg() {
    let dir = tempfile::tempdir().unwrap();
    let conda = dir.path().join("miniconda3");
    write(&conda, "envs/proj/conda-meta/python-3.12.4-h14ffc60_0.json", "{}");
    fs::create_dir_all(conda.join("envs/proj/Lib/site-packages/numpy-1.26.0.dist-info")).unwrap();
    let env = python_env(&conda.join("envs/proj"), Origin::Override).unwrap();
    assert_eq!(env.kind, EnvKind::Conda);
    assert_eq!(env.version.as_deref(), Some("3.12"));
    assert!(accepts_env_path(&conda.join("envs/proj")));
    // environment.yml name + CONDA_EXE base.
    let root = dir.path().join("repo");
    write(&root, "environment.yml", "name: proj\ndependencies:\n  - numpy\n");
    let exe = conda.join("Scripts").join("conda.exe");
    write(&conda, "Scripts/conda.exe", "");
    let p = platform(Os::Windows);
    let vars = EnvVars::from_pairs(&[("CONDA_EXE", exe.to_str().unwrap())]);
    let found = find_environment(&cx(&root, &p, &vars, None)).unwrap().unwrap();
    assert!(found.root.ends_with("proj"));
    assert_eq!(hint(&root), "conda env create -f environment.yml");
}

#[test]
fn rule_poetry_env_dir_name_hash() {
    // Poetry's own algorithm: sha256 of the normalised path, URL-safe base64, 8 chars.
    let name = poetry_env_name("My Project", "c:\\work\\proj");
    let digest = Sha256::digest(b"c:\\work\\proj");
    assert_eq!(name, format!("my_project-{}", &base64_urlsafe(&digest)[..8]));
    assert_eq!(base64_urlsafe(b"\xfb\xff\xfe"), "-__-");
    assert_eq!(base64_urlsafe(b"ab"), "YWI=");
    assert!(poetry_env_name(&"x".repeat(60), "/p").starts_with(&format!("{}-", "x".repeat(42))));
    // The environment is found under POETRY_VIRTUALENVS_PATH by that name.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    write(&root, "pyproject.toml", "[tool.poetry]\nname = \"demo\"\n");
    let p = platform(if cfg!(windows) { Os::Windows } else { Os::Linux });
    let env_name = poetry_env_name("demo", &poetry_normcase(&root, &p));
    let base = dir.path().join("venvs");
    venv(&base, &format!("{env_name}-py3.12"), "3.12", &[]);
    let vars = EnvVars::from_pairs(&[("POETRY_VIRTUALENVS_PATH", base.to_str().unwrap())]);
    let found = find_environment(&cx(&root, &p, &vars, None)).unwrap().unwrap();
    assert_eq!(found.origin, Origin::UserCache);
    assert!(found.root.to_string_lossy().ends_with("-py3.12"));
}

#[test]
fn rule_declared_python_deps_missing_are_listed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "pyproject.toml",
        "[project]\nname = \"app\"\nrequires-python = \">=3.10\"\ndependencies = [\"Flask>=3\", \"click\", \"app-core\"]\n[dependency-groups]\ntests = [\"pytest\"]\n",
    );
    venv(root, ".venv", "3.12", &["flask-3.0.0.dist-info"]);
    let p = platform(Os::Linux);
    let vars = EnvVars::default();
    let s = setup(&cx(root, &p, &vars, None));
    assert_eq!(s.deps.status, DepsStatus::Missing);
    assert_eq!(s.deps.missing, vec!["app-core".to_string(), "click".to_string()]);
    assert!(s.deps.notes.iter().any(|n| n.contains("pytest")), "{:?}", s.deps.notes);
    assert_eq!(s.deps.hint, "pip install -e .");
    assert!(s.deps.roots.iter().any(|r| r.layout == "site_packages"));
    // Installing the missing packages satisfies the check and changes the fingerprint.
    let before = s.deps.fingerprint.clone();
    for d in ["click-8.1.7.dist-info", "app_core-0.1.0.dist-info"] {
        fs::create_dir_all(root.join(".venv/Lib/site-packages").join(d)).unwrap();
    }
    let s = setup(&cx(root, &p, &vars, None));
    assert_eq!(s.deps.status, DepsStatus::Installed);
    assert_ne!(s.deps.fingerprint, before);
    // No manifest: nothing declared.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(setup(&cx(empty.path(), &p, &vars, None)).deps.status, DepsStatus::NoneDeclared);
}

#[test]
fn rule_markers_exclude_other_platforms() {
    let win = pep508::MarkerEnv::new(&platform(Os::Windows), Some("3.12"));
    let linux = pep508::MarkerEnv::new(&platform(Os::Linux), Some("3.12"));
    let parsed = pep508::parse("pywin32>=306; sys_platform == \"win32\"").unwrap();
    assert_eq!(parsed.name, "pywin32");
    let m = parsed.marker.unwrap();
    assert!(pep508::evaluate(&m, &win));
    assert!(!pep508::evaluate(&m, &linux));
    let m = pep508::parse_marker("python_version < \"3.11\" and os_name == 'posix'").unwrap();
    assert!(!pep508::evaluate(&m, &linux));
    let m =
        pep508::parse_marker("(python_version >= '3.8' or extra == 'x') and platform_system != 'Windows'")
            .unwrap();
    assert!(pep508::evaluate(&m, &linux));
    assert!(!pep508::evaluate(&m, &win));
    let m = pep508::parse_marker("extra == \"test\"").unwrap();
    assert!(!pep508::evaluate(&m, &linux), "extras are not base requirements");
    // Unknown Python version: the requirement applies (never dropped on a guess).
    let unknown = pep508::MarkerEnv::new(&platform(Os::Linux), None);
    let m = pep508::parse_marker("python_version < '3.11'").unwrap();
    assert!(pep508::evaluate(&m, &unknown));
    assert!(pep508::parse("git+https://example.org/x.git").is_none());
    assert!(pep508::parse("./local/pkg").is_none());
    assert_eq!(pep508::parse("requests[socks] @ https://x/y.whl").unwrap().name, "requests");
    assert_eq!(normalize("Foo_Bar.baz"), "foo-bar-baz");
}

#[test]
fn rule_python_hint_follows_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "requirements.txt", "-r base.txt\nrequests  # http\n");
    write(root, "base.txt", "six\n");
    assert_eq!(hint(root), "pip install -r requirements.txt");
    write(root, "Pipfile", "[packages]\nrequests = \"*\"\n");
    assert_eq!(hint(root), "pipenv install --dev");
    write(root, "poetry.lock", "");
    assert_eq!(hint(root), "poetry install");
    write(root, "uv.lock", "");
    assert_eq!(hint(root), "uv sync");
    let m = Manifests::read(root);
    let names: BTreeSet<String> = m.requirements.iter().map(|r| r.text.clone()).collect();
    assert!(names.contains("six") && names.contains("requests"), "{names:?}");
}

#[test]
fn rule_nested_examples_are_subprojects() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "pyproject.toml", "[project]\nname = \"flaskish\"\ndependencies = []\n");
    write(root, "examples/celery/pyproject.toml", "[project]\nname = \"ex\"\ndependencies = [\"celery\"]\n");
    write(root, "packages/core/pyproject.toml", "[project]\nname = \"core\"\ndependencies = []\n");
    write(root, "pyproject.toml", "[project]\nname = \"flaskish\"\ndependencies = []\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\n");
    venv(root, ".venv", "3.12", &[]);
    let p = platform(Os::Linux);
    let vars = EnvVars::default();
    let s = setup(&cx(root, &p, &vars, None));
    let dirs: Vec<&str> = s.deps.subprojects.iter().map(|s| s.dir.as_str()).collect();
    assert_eq!(dirs, vec!["examples/celery"], "workspace members are not sub-projects");
    assert_ne!(s.deps.status, DepsStatus::Missing, "a sub-project never fails the index");
    assert!(s
        .deps
        .notes
        .iter()
        .any(|n| n.starts_with("examples/celery") && n.contains("celery")));
}

#[test]
fn rule_setup_py_literal_install_requires_is_read_from_the_tree() {
    let src = b"from setuptools import setup\nsetup(name='x', install_requires=['requests>=2', \"click\"], extras_require={'a': ['b']})\n";
    assert_eq!(setup_py_install_requires(src), vec!["requests>=2".to_string(), "click".to_string()]);
    assert!(setup_py_install_requires(b"setup(install_requires=REQS)\n").is_empty());
    assert_eq!(lower_bound(">=3.9,<4"), Some("3.9".into()));
    assert_eq!(lower_bound("^3.10"), Some("3.10".into()));
}
