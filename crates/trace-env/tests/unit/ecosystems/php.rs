use super::*;
use crate::os::{EnvVars, Platform};
use crate::test_support::write;

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

#[test]
fn rule_composer_installed_json_satisfies_require() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "composer.json",
        r#"{"name":"guzzlehttp/guzzle","require":{"php":"^7.2.5 || ^8.0","ext-json":"*","psr/http-message":"^1.1","guzzlehttp/psr7":"^2"},"require-dev":{"phpunit/phpunit":"^9"}}"#,
    );
    write(root, "vendor-bin/php-cs-fixer/composer.json", r#"{"require":{"friendsofphp/php-cs-fixer":"3"}}"#);
    let p = Platform::current();
    let vars = EnvVars::default();
    // Nothing installed: every non-platform package is missing (dev included).
    let s = setup(&cx(root, &p, &vars, None), None);
    assert_eq!(s.deps.status, DepsStatus::Missing);
    assert_eq!(s.deps.missing, vec!["guzzlehttp/psr7", "phpunit/phpunit", "psr/http-message"]);
    assert_eq!(s.deps.hint, "composer install");
    assert_eq!(s.extensions, vec!["json"]);
    assert_eq!(s.php_version, "8.4.0");
    let subs: Vec<&str> = s.deps.subprojects.iter().map(|s| s.dir.as_str()).collect();
    assert_eq!(subs, vec!["vendor-bin/php-cs-fixer"]);
    // Composer 2 installed.json: names, replace and provide satisfy requirements.
    write(
        root,
        "vendor/composer/installed.json",
        r#"{"packages":[{"name":"guzzlehttp/psr7","provide":{"psr/http-message-implementation":"1.0"}},{"name":"psr/http-message"},{"name":"phpunit/phpunit"}],"dev":true}"#,
    );
    let s = setup(&cx(root, &p, &vars, None), None);
    assert_eq!(s.deps.status, DepsStatus::Installed, "{:?}", s.deps.missing);
    assert!(s.vendor_installed);
    assert_eq!(s.deps.roots[0].layout, "php_vendor");
    // A no-dev install: missing dev packages are a status line only.
    write(
        root,
        "vendor/composer/installed.json",
        r#"{"packages":[{"name":"guzzlehttp/psr7"},{"name":"psr/http-message"}],"dev":false}"#,
    );
    let s = setup(&cx(root, &p, &vars, None), None);
    assert_eq!(s.deps.status, DepsStatus::Installed);
    assert!(s.deps.notes.iter().any(|n| n.contains("phpunit/phpunit")));
    assert!(accepts_env_path(&root.join("vendor")));
}

#[test]
fn rule_php_version_follows_platform_then_toolchain_then_require() {
    let composer: Value =
        serde_json::from_str(r#"{"config":{"platform":{"php":"8.1"}},"require":{"php":"^7.4"}}"#).unwrap();
    assert_eq!(php_version(&composer, None), "8.1.0");
    let composer: Value = serde_json::from_str(r#"{"require":{"php":"^7.4"}}"#).unwrap();
    assert_eq!(php_version(&composer, None), "7.4.0");
    let composer: Value = serde_json::from_str(r#"{"require":{"php":">=7.2"}}"#).unwrap();
    assert_eq!(php_version(&composer, None), NEWEST_PHP);
    assert_eq!(
        version_from_path(Path::new("C:/Users/u/AppData/Local/Microsoft/WinGet/Packages/PHP.PHP.8.4_Microsoft.Winget.Source_8wekyb3d8bbwe/php.exe"))
            .map(|v| v.parts),
        Some(vec![8, 4])
    );
    assert_eq!(version_from_path(Path::new("/usr/bin/php8.3")).map(|v| v.parts), Some(vec![8, 3]));
}
