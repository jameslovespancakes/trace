use super::*;

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

#[test]
fn rule_gopls_is_chosen_by_go_minor() {
    let versions = vec![
        GoInstallVersion {
            min_go: "1.25".into(),
            version: "v0.23.0".into(),
            h1: String::new(),
        },
        GoInstallVersion {
            min_go: "1.23".into(),
            version: "v0.21.1".into(),
            h1: String::new(),
        },
        GoInstallVersion {
            min_go: "1.22".into(),
            version: "v0.20.0".into(),
            h1: String::new(),
        },
    ];
    assert_eq!(choose_go_version(&versions, Some(&v("go1.27.0"))).unwrap().version, "v0.23.0");
    assert_eq!(choose_go_version(&versions, Some(&v("1.24.5"))).unwrap().version, "v0.21.1");
    assert_eq!(choose_go_version(&versions, Some(&v("1.22.0"))).unwrap().version, "v0.20.0");
    assert!(choose_go_version(&versions, Some(&v("1.21.9"))).is_none());
    assert_eq!(choose_go_version(&versions, None).unwrap().version, "v0.20.0");
    assert_eq!(go_binary_name("golang.org/x/tools/gopls"), "gopls");
    assert_eq!(go_binary_name("example.com/tool/v2"), "tool");
    assert_eq!(escape_module_path("github.com/BurntSushi/toml"), "github.com/!burnt!sushi/toml");
}

/// Install commands get an allow-listed environment only: never a user's secrets, TEMP/TMP
/// pointing into the scratch directory.
#[test]
fn rule_install_commands_never_see_secrets() {
    let vars = EnvVars::from_pairs(&[
        ("PATH", "/usr/bin"),
        ("MY_SERVICE_TOKEN", "secret"),
        ("GOFLAGS", "-mod=mod"),
        ("HTTPS_PROXY", "http://proxy:3128"),
    ]);
    let tmp = PathBuf::from("scratch");
    let env = base_env(&vars, &tmp);
    assert!(!env.contains_key("MY_SERVICE_TOKEN"));
    assert!(!env.contains_key("GOFLAGS"), "the user's Go flags never leak into installs");
    assert_eq!(env.get("TMPDIR"), Some(&OsString::from("scratch")));
    assert!(env.contains_key("HTTPS_PROXY"));
}

#[test]
fn rule_install_command_output_goes_to_the_log() {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-eco-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).unwrap();
    let log = dir.join("logs").join("x.log");
    // A program that surely exists and fails: the test binary itself with a bad flag.
    let me = std::env::current_exe().unwrap();
    let (ok, _) = run_logged(
        &me,
        &os_args(&[&"--definitely-not-a-flag"]),
        &base_env(&EnvVars::from_process(), &dir),
        &dir,
        &log,
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(!ok);
    let text = fs::read_to_string(&log).unwrap();
    assert!(text.starts_with("$ ") && text.contains("--definitely-not-a-flag"), "{text}");
    let _ = fs::remove_dir_all(&dir);
}
