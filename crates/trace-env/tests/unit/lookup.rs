use super::*;
use std::fs;

fn write_exe(dir: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let file = dir.join(name);
    fs::write(&file, b"binary").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    file
}

/// One lookup order for every search: steps are searched in `ORDER` whatever order the
/// caller adds them in; forbidden roots are skipped.
#[test]
fn rule_lookup_follows_the_one_order() {
    let tmp = tempfile::tempdir().unwrap();
    let p = Platform::current();
    let (project, path, tools) =
        (tmp.path().join("project"), tmp.path().join("path"), tmp.path().join("tools"));
    for dir in [&project, &path, &tools] {
        write_exe(dir, &p.exe("prog"));
    }
    let none: Vec<PathBuf> = Vec::new();
    let lookup = Lookup::new(&p, &none)
        .with(Where::Tools, [tools.clone()])
        .with(Where::Path, [path.clone()])
        .with(Where::Project, [project.clone()]);
    assert_eq!(lookup.find(&["prog"]), Some((project.join(p.exe("prog")), Where::Project)));
    let steps: Vec<Where> = lookup.dirs().map(|(w, _)| w).collect();
    assert_eq!(steps, vec![Where::Project, Where::Path, Where::Tools]);
    let forbidden = vec![project.clone()];
    let lookup = Lookup::new(&p, &forbidden)
        .with(Where::Tools, [tools.clone()])
        .with(Where::Project, [project]);
    assert_eq!(lookup.find(&["prog"]), Some((tools.join(p.exe("prog")), Where::Tools)));
}

/// mise / asdf install folders (standard locations of the toolchains they install):
/// `MISE_DATA_DIR` / `ASDF_DATA_DIR` when set, else the defaults under the home folder;
/// version folders only, newest first.
#[test]
fn rule_version_manager_installs_are_found() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let p = Platform {
        os: Os::Linux,
        ..Platform::current()
    };
    for v in ["1.22.1", "1.23.4", "latest"] {
        fs::create_dir_all(home.join(".local/share/mise/installs/go").join(v)).unwrap();
    }
    fs::create_dir_all(home.join(".asdf/installs/golang/1.21.0/go")).unwrap();
    let h = home.display().to_string();
    let vars = EnvVars::from_pairs(&[("HOME", h.as_str())]);
    assert_eq!(
        mise_installs(&vars, &p, "go"),
        vec![
            home.join(".local/share/mise/installs/go/1.23.4"),
            home.join(".local/share/mise/installs/go/1.22.1")
        ]
    );
    assert_eq!(asdf_installs(&vars, &p, "golang"), vec![home.join(".asdf/installs/golang/1.21.0")]);
    let data = tmp.path().join("data");
    fs::create_dir_all(data.join("installs/go/1.24.0")).unwrap();
    let d = data.display().to_string();
    let vars = EnvVars::from_pairs(&[("HOME", h.as_str()), ("MISE_DATA_DIR", d.as_str())]);
    assert_eq!(mise_installs(&vars, &p, "go"), vec![data.join("installs/go/1.24.0")]);
}
