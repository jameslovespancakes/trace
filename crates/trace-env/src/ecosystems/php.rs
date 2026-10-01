//! PHP: Composer `vendor/` detection and the declared-vs-installed dependency check, plus the
//! optional PHP toolchain. Read-only: `composer.json`, `composer.lock` and
//! `<vendor>/composer/installed.json`; Composer and PHP never run on the project.
//!
//! PHP itself is optional (Intelephense bundles the PHP stubs); it only refines the PHP
//! version given to the server: `composer.json` `config.platform.php` > the installed PHP >
//! the highest PHP major allowed by `require.php` (Intelephense's newest supported minor).
//!
//! Required: `require` + `require-dev` minus platform packages (`php`, `ext-*`, `lib-*`,
//! `composer-*-api`, ...). A name is satisfied by an installed package's `name`, `replace`
//! or `provide`. `require-dev` is required unless `installed.json` says the install was done
//! without dev packages (`"dev": false`), in which case missing dev packages are a status line.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::lookup::Lookup;
use crate::os::{self, Os, Version};
use crate::Where;
use crate::{
    hex, subdirs, DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot, Origin,
    SubProject, Toolchain, ToolchainStatus, SKIP_DIRS,
};

/// Newest PHP version Intelephense 1.18 models (used when only `require.php` bounds it).
pub(crate) const NEWEST_PHP: &str = "8.4.0";
/// Maximum directory depth searched for nested Composer projects.
const PROJECT_DEPTH: usize = 4;

/// Everything the PHP preflight needs, computed once.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PhpSetup {
    /// Absolute vendor directory (`config.vendor-dir`, default `vendor`, or `--env`).
    pub vendor_dir: PathBuf,
    /// `<vendor>/composer/installed.json` exists.
    pub vendor_installed: bool,
    /// PHP version for the server ("8.3.0").
    pub php_version: String,
    /// `ext-*` names the project requires (without the prefix, as written).
    pub extensions: Vec<String>,
    /// `--env` pointed to a directory that is no Composer vendor directory.
    pub env_not_found: Option<PathBuf>,
    pub deps: DepsReport,
}

/// The PHP interpreter, found without running project code (PATH, then the standard
/// install locations of the OS). Optional for trace.
pub fn toolchain(cx: &DetectContext<'_>) -> ToolchainStatus {
    let found = Lookup::new(cx.platform, &[])
        .with_path(cx.vars)
        .with(Where::Standard, standard_dirs(cx))
        .find(&["php"]);
    match found {
        Some((exe, step)) => {
            let root = exe.parent().map(Path::to_path_buf).unwrap_or_default();
            let version = version_from_path(&exe).or_else(|| {
                os::toolchain_output(&exe, &["-n", "-v"]).and_then(|text| {
                    let rest = text.trim_start().strip_prefix("PHP ")?;
                    Version::parse(rest.split_whitespace().next()?)
                })
            });
            let origin = if step == Where::Path {
                Origin::Path
            } else {
                Origin::StandardLocation
            };
            let mut executables = std::collections::BTreeMap::new();
            executables.insert("php".to_string(), exe);
            ToolchainStatus::Found(Toolchain {
                id: "php",
                root,
                version,
                executables,
                origin,
                facts: Default::default(),
            })
        }
        None => ToolchainStatus::Missing {
            searched: vec!["PATH (php)".into(), "standard PHP install locations".into()],
        },
    }
}

/// Declared Composer packages against `vendor/composer/installed.json`.
pub fn deps(cx: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
    setup(cx, toolchain).deps
}

/// A Composer vendor directory (`composer/installed.json` inside).
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    path.join("composer").join("installed.json").is_file()
}

/// Standard PHP install directories per OS (versioned ones newest first).
fn standard_dirs(cx: &DetectContext<'_>) -> Vec<PathBuf> {
    let home = os::home_dir(cx.vars, cx.platform);
    let mut out = Vec::new();
    match cx.platform.os {
        Os::Windows => {
            if let Some(local) = cx.vars.path("LOCALAPPDATA") {
                let packages = local.join("Microsoft").join("WinGet").join("Packages");
                out.extend(versioned(&packages, "PHP.PHP."));
            }
            out.push(PathBuf::from(r"C:\php"));
            out.push(PathBuf::from(r"C:\xampp\php"));
            out.extend(versioned(Path::new(r"C:\laragon\bin\php"), "php-"));
            if let Some(h) = &home {
                out.push(h.join("scoop").join("apps").join("php").join("current"));
            }
            out.extend(versioned(Path::new(r"C:\tools"), "php"));
        }
        Os::MacOs => {
            out.push(PathBuf::from("/opt/homebrew/bin"));
            for (_, p) in os::versioned_children(Path::new("/opt/homebrew/opt"), "php@") {
                out.push(p.join("bin"));
            }
            out.push(PathBuf::from("/usr/local/bin"));
        }
        Os::Linux => {
            out.push(PathBuf::from("/usr/bin"));
            out.push(PathBuf::from("/usr/local/bin"));
        }
    }
    out
}

fn versioned(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    os::versioned_children(dir, prefix)
        .into_iter()
        .map(|(_, p)| p)
        .collect()
}

/// Version from install paths: `PHP.PHP.8.4_Microsoft.Winget...`, `php-8.3.6-Win32-vs16-x64`,
/// `php8.3`, `Cellar/php/8.4.1/bin/php`, `php@8.2`.
fn version_from_path(exe: &Path) -> Option<Version> {
    for component in exe
        .ancestors()
        .filter_map(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
    {
        let lower = component.to_lowercase();
        for prefix in ["php.php.", "php-", "php@", "php"] {
            if let Some(rest) = lower.strip_prefix(prefix) {
                if rest.starts_with(|c: char| c.is_ascii_digit()) {
                    let text: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                    if let Some(v) = Version::parse(text.trim_end_matches('.')) {
                        return Some(v);
                    }
                }
            }
        }
        // Homebrew `Cellar/php/<version>`.
        if component.starts_with(|c: char| c.is_ascii_digit())
            && exe.to_string_lossy().replace('\\', "/").contains("/Cellar/php")
        {
            if let Some(v) = Version::parse(&component) {
                return Some(v);
            }
        }
    }
    None
}

/// Platform packages are provided by PHP itself, never by Composer.
fn is_platform_package(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "php"
            | "php-64bit"
            | "php-ipv6"
            | "php-zts"
            | "php-debug"
            | "hhvm"
            | "composer"
            | "composer-plugin-api"
            | "composer-runtime-api"
    ) || n.starts_with("ext-")
        || n.starts_with("lib-")
}

/// Vendor directory, PHP version, extensions and the dependency report.
pub fn setup(cx: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> PhpSetup {
    let root = cx.root;
    let composer_bytes = fs::read(root.join("composer.json")).unwrap_or_default();
    let composer: Value = serde_json::from_slice(&composer_bytes).unwrap_or(Value::Null);
    let configured = composer
        .get("config")
        .and_then(|c| c.get("vendor-dir"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            cx.vars
                .get("COMPOSER_VENDOR_DIR")
                .and_then(|v| v.to_str())
                .filter(|v| !v.trim().is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "vendor".to_string());
    let mut vendor_dir = {
        let p = PathBuf::from(&configured);
        if p.is_absolute() {
            p
        } else {
            root.join(p)
        }
    };
    let mut env_not_found = None;
    if let Some(path) = cx.env_override {
        if accepts_env_path(path) {
            vendor_dir = path.to_path_buf();
        } else {
            env_not_found = Some(path.to_path_buf());
        }
    }
    let installed_bytes = fs::read(vendor_dir.join("composer").join("installed.json")).ok();
    let vendor_installed = installed_bytes.is_some() && cx.allowed(&vendor_dir);
    let installed: Value = installed_bytes
        .as_deref()
        .and_then(|b| serde_json::from_slice(b).ok())
        .unwrap_or(Value::Null);
    let (names, dev_installed) = installed_names(&installed);

    let require = string_keys(composer.get("require"));
    let require_dev = string_keys(composer.get("require-dev"));
    let mut extensions: Vec<String> = require
        .iter()
        .chain(require_dev.iter())
        .filter_map(|n| n.strip_prefix("ext-").map(str::to_string))
        .collect();
    extensions.sort();
    extensions.dedup();

    let php_version = php_version(&composer, toolchain);
    let own = composer
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let wanted = |list: &[String]| -> Vec<String> {
        list.iter()
            .filter(|n| !is_platform_package(n))
            .filter(|n| Some(n.to_ascii_lowercase()) != own)
            .filter(|n| !vendor_installed || !names.contains(&n.to_ascii_lowercase()))
            .cloned()
            .collect()
    };
    let missing_runtime = wanted(&require);
    let missing_dev = wanted(&require_dev);
    let declared = require
        .iter()
        .chain(require_dev.iter())
        .any(|n| !is_platform_package(n));
    let hint = "composer install".to_string();
    let mut missing = missing_runtime.clone();
    let mut notes = Vec::new();
    // Without an install, or after a dev install, require-dev is required too.
    if !vendor_installed || dev_installed {
        missing.extend(missing_dev);
    } else if !missing_dev.is_empty() {
        notes.push(format!("dev dependencies not installed: {} (composer install)", missing_dev.join(", ")));
    }
    missing.sort();
    missing.dedup();
    let status = if !missing.is_empty() {
        DepsStatus::Missing
    } else if declared {
        DepsStatus::Installed
    } else {
        DepsStatus::NoneDeclared
    };
    let subprojects: Vec<SubProject> = composer_dirs(root, &vendor_dir)
        .into_iter()
        .filter(|d| !d.is_empty() && cx.allowed(&root.join(d)))
        .map(|dir| SubProject {
            dir,
            reason: "separate Composer project (composer.json)".into(),
        })
        .collect();
    let roots = if vendor_installed {
        vec![LibraryRoot {
            path: vendor_dir.clone(),
            kind: LibraryKind::Dependency,
            ecosystem: EcosystemId::Php,
            layout: "php_vendor",
            version: None,
        }]
    } else {
        Vec::new()
    };
    let mut h = blake3::Hasher::new();
    h.update(&composer_bytes);
    h.update(&fs::read(root.join("composer.lock")).unwrap_or_default());
    h.update(vendor_dir.to_string_lossy().as_bytes());
    h.update(installed_bytes.as_deref().unwrap_or_default());
    h.update(php_version.as_bytes());
    PhpSetup {
        vendor_dir,
        vendor_installed,
        php_version,
        extensions,
        env_not_found,
        deps: DepsReport {
            status,
            missing,
            hint,
            roots,
            fingerprint: hex(h),
            subprojects,
            notes,
        },
    }
}

fn string_keys(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// Lower-cased names satisfied by the install (`name`, `replace`, `provide`), and whether
/// the install included dev packages. Composer 2 `{"packages": [...], "dev": bool}`,
/// Composer 1 a plain array.
fn installed_names(installed: &Value) -> (BTreeSet<String>, bool) {
    let (packages, dev) = match installed {
        Value::Array(a) => (a.as_slice(), true),
        Value::Object(o) => (
            o.get("packages")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            o.get("dev").and_then(Value::as_bool).unwrap_or(true),
        ),
        _ => (&[][..], true),
    };
    let mut names = BTreeSet::new();
    for p in packages {
        if let Some(n) = p.get("name").and_then(Value::as_str) {
            names.insert(n.to_ascii_lowercase());
        }
        for key in ["replace", "provide"] {
            if let Some(o) = p.get(key).and_then(Value::as_object) {
                names.extend(o.keys().map(|k| k.to_ascii_lowercase()));
            }
        }
    }
    (names, dev)
}

/// `config.platform.php` > the installed PHP > the newest version `require.php` allows.
fn php_version(composer: &Value, toolchain: Option<&Toolchain>) -> String {
    if let Some(v) = composer
        .get("config")
        .and_then(|c| c.get("platform"))
        .and_then(|p| p.get("php"))
        .and_then(Value::as_str)
        .and_then(Version::parse)
    {
        return full_version(&v);
    }
    if let Some(v) = toolchain.and_then(|t| t.version.as_ref()) {
        return full_version(v);
    }
    let Some(req) = composer
        .get("require")
        .and_then(|r| r.get("php"))
        .and_then(Value::as_str)
    else {
        return NEWEST_PHP.to_string();
    };
    // The highest major named by the constraint; its newest minor Intelephense models.
    let newest = Version::parse(NEWEST_PHP).map(|v| v.parts[0]).unwrap_or(8);
    let highest = req
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter_map(Version::parse)
        .map(|v| v.parts[0])
        .max();
    let unbounded = req.contains(">=") || req.contains('>') && !req.contains('<') || req.trim() == "*";
    match highest {
        Some(major) if major < newest && !unbounded => format!("{major}.4.0"),
        _ => NEWEST_PHP.to_string(),
    }
}

/// "8.3" -> "8.3.0".
fn full_version(v: &Version) -> String {
    let part = |i: usize| v.parts.get(i).copied().unwrap_or(0);
    format!("{}.{}.{}", part(0), part(1), part(2))
}

/// Directories (relative, `""` = root) with a `composer.json`, outside the vendor dir.
fn composer_dirs(root: &Path, vendor: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if dir.join("composer.json").is_file() {
            out.push(rel.clone());
        }
        if depth >= PROJECT_DEPTH {
            continue;
        }
        for (name, path) in subdirs(&dir) {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) || path == vendor {
                continue;
            }
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            stack.push((path, child, depth + 1));
        }
    }
    out.sort();
    out
}

/// The Php ecosystem ([`crate::Ecosystem`]).
pub struct Php;

impl crate::Ecosystem for Php {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::Php
    }

    fn accepts_env_path(&self, path: &Path) -> bool {
        accepts_env_path(path)
    }

    fn toolchain(&self, cx: &DetectContext<'_>) -> ToolchainStatus {
        toolchain(cx)
    }

    fn deps(&self, read: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
        deps(read, toolchain)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/ecosystems/php.rs"]
mod tests;
