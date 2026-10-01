use super::*;

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    std::fs::write(&path, text).expect("write");
}

/// Site-packages with the activating distribution `runner` (its provider module and the
/// class it constructs), a plugin requiring it and a package requiring it only as an
/// extra.
fn site() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    write(d, "runner-1.0.dist-info/METADATA", "Metadata-Version: 2.1\nName: runner\nVersion: 1.0\n\nbody\n");
    write(
        d,
        "runner-1.0.dist-info/RECORD",
        "_runner/__init__.py,,\n_runner/patch.py,,\n_runner/helpers.py,,\nrunner-1.0.dist-info/METADATA,,\n../../Scripts/runner.exe,,\n",
    );
    write(d, "_runner/__init__.py", "");
    write(d, "_runner/helpers.py", "class Patcher:\n    def setenv(self, name, value):\n        pass\n");
    write(
        d,
        "_runner/patch.py",
        "from _runner.helpers import Patcher\nfrom _runner.fixtures import provider\n\n\
@provider\ndef patcher():\n    p = Patcher()\n    yield p\n    p.undo()\n\n\
@provider(name=\"renamed\")\ndef make_renamed():\n    return Patcher()\n\n\
@provider\ndef config(request):\n    return request.config\n\n\
@provider\ndef passthrough(value):\n    yield value\n\n\
def helper():\n    return Patcher()\n",
    );
    write(
        d,
        "plugin_a-2.0.dist-info/METADATA",
        "Name: plugin-a\nRequires-Dist: runner (>=1.0)\nRequires-Dist: other; python_version < \"3.11\"\n",
    );
    write(d, "plugin_a-2.0.dist-info/RECORD", "plugin_a.py,,\n");
    write(d, "plugin_a.py", "import runner\n\nclass Session:\n    pass\n\n@runner.provider\ndef session():\n    return Session()\n");
    write(d, "tooling-1.0.dist-info/METADATA", "Name: tooling\nRequires-Dist: runner; extra == \"test\"\n");
    write(d, "tooling-1.0.dist-info/RECORD", "tooling.py,,\n");
    write(d, "tooling.py", "import runner\n\n@runner.provider\ndef patcher():\n    return 1\n");
    dir
}

/// Providers of the activating distribution and of its plugins are found under the name
/// they provide; a distribution requiring the package only as an extra is no plugin;
/// undecorated functions provide nothing.
#[test]
fn rule_installed_plugin_providers_are_found() {
    let dir = site();
    let roots = vec![dir.path().to_path_buf()];
    let found = installed_providers(&FsLoader, &roots, &roots, "runner", "runner.provider", Some("name"));
    let names: Vec<&str> = found.keys().map(String::as_str).collect();
    assert_eq!(names, vec!["config", "passthrough", "patcher", "renamed", "session"]);
    assert_eq!(found["patcher"].len(), 1, "the extra-only package is no plugin: {found:?}");
    assert_eq!(found["session"][0].symbol, "plugin_a.session");
    assert!(installed_providers(&FsLoader, &roots, &roots, "absent", "absent.provider", None).is_empty());
}

/// The value of an installed provider is the library class its source constructs, for a
/// generator what it yields; a value the source does not construct (an attribute read,
/// a parameter passed through) has no class.
#[test]
fn rule_installed_provider_value_is_the_constructed_class() {
    let dir = site();
    let roots = vec![dir.path().to_path_buf()];
    let found = installed_providers(&FsLoader, &roots, &roots, "runner", "runner.provider", Some("name"));
    let value = |name: &str| found[name][0].value.clone();
    assert_eq!(value("patcher").as_deref(), Some("_runner.helpers.Patcher"));
    assert_eq!(value("renamed").as_deref(), Some("_runner.helpers.Patcher"));
    assert_eq!(value("session").as_deref(), Some("plugin_a.Session"));
    assert_eq!(value("config"), None);
    assert_eq!(value("passthrough"), None);
}

/// Installed providers are read again only when the installation changes: the key of an
/// unchanged installation is stable, a new distribution changes it.
#[test]
fn rule_installed_providers_key_follows_the_installation() {
    let dir = site();
    let roots = vec![dir.path().to_path_buf()];
    let rows: Rows = vec![("runner".into(), "runner.provider".into(), Some("name".into()))];
    let before = installation_key(&rows, &roots, &roots);
    assert_eq!(before, installation_key(&rows, &roots, &roots));
    write(
        dir.path(),
        "plugin_b-1.0.dist-info/METADATA",
        "Name: plugin-b
Requires-Dist: runner
",
    );
    write(
        dir.path(),
        "plugin_b-1.0.dist-info/RECORD",
        "plugin_b.py,,
",
    );
    assert_ne!(before, installation_key(&rows, &roots, &roots));
}

#[test]
fn rule_installed_plugin_metadata_requirements() {
    let (name, requires) = parse_metadata(
        "Name: x-y\nRequires-Dist: runner>=1\nRequires-Dist: a.b [c] (>=2)\nRequires-Dist: t; extra == 'dev'\n\nRequires-Dist: body\n",
    );
    assert_eq!(name, "x-y");
    assert_eq!(requires, vec!["runner", "a.b"]);
}
