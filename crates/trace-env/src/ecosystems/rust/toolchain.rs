//! The Rust toolchain: `--env`, pins (`rust-toolchain[.toml]`, rustup directory overrides),
//! rustup toolchains, PATH and the standard install folders; the host triple and `rust-src`.

use super::*;

/// Outcome of the toolchain search.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolution {
    pub status: ToolchainStatus,
    /// The project pins a toolchain (`rust-toolchain.toml`) that is not installed:
    /// (channel, pin file relative to the root or absolute when above it).
    pub pinned_missing: Option<(String, String)>,
}

/// `toolchain` of the ecosystem contract: [`resolve`] without the pin detail.
pub fn toolchain(cx: &DetectContext<'_>) -> ToolchainStatus {
    resolve(cx).status
}

/// The Rust toolchain for this repository (module docs), MSRV-checked.
pub fn resolve(cx: &DetectContext<'_>) -> Resolution {
    let mut searched = Vec::new();
    let home = os::home_dir(cx.vars, cx.platform);
    let cargo_home = cargo_home(cx);
    let rustup_home = cx
        .vars
        .path("RUSTUP_HOME")
        .or_else(|| home.as_ref().map(|h| h.join(".rustup")));
    let settings = rustup_home
        .as_ref()
        .and_then(|h| relpath::read_toml(&h.join("settings.toml")))
        .unwrap_or(Value::Null);
    let host = settings
        .get("default_host_triple")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| host_triple(cx.platform));

    let mut found: Option<Toolchain> = None;
    let mut pinned_missing = None;

    // 1. `--env <toolchain root>`.
    if let Some(dir) = cx.env_override.filter(|d| is_toolchain_root(d, cx.platform)) {
        // A toolchain inside rustup's `toolchains/` dir keeps its rustup name (host triple,
        // `--toolchain` in the fix texts).
        let rustup_name = dir
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("toolchains")))
            .and_then(|_| dir.file_name())
            .and_then(|n| n.to_str())
            .filter(|n| triple_suffix(n).is_some());
        found = toolchain_at(dir, Origin::Override, rustup_name, &host, cx.platform);
        searched.push(format!("--env {}", dir.display()));
    }

    // 2. rustup: pin -> directory override -> RUSTUP_TOOLCHAIN -> default.
    if found.is_none() {
        if let Some(rustup_home) = rustup_home.as_ref().filter(|h| cx.allowed(h)) {
            let toolchains = rustup_home.join("toolchains");
            let pin = pin_file(cx.root, cx);
            let (name, origin, pin_source) = match &pin {
                Some((channel, source)) => (Some(channel.clone()), Origin::Pin, Some(source.clone())),
                None => match directory_override(&settings, cx.root) {
                    Some(name) => (Some(name), Origin::Pin, Some("rustup override".to_string())),
                    None => match cx.vars.get("RUSTUP_TOOLCHAIN").and_then(|v| v.to_str()) {
                        Some(name) if !name.trim().is_empty() => {
                            (Some(name.trim().to_string()), Origin::Override, None)
                        }
                        _ => (
                            settings
                                .get("default_toolchain")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            Origin::UserCache,
                            None,
                        ),
                    },
                },
            };
            if let Some(name) = name {
                searched.push(format!("rustup toolchain {name} ({})", toolchains.display()));
                match toolchain_dir(&toolchains, &name, &host) {
                    Some((dir_name, dir)) => {
                        found = toolchain_at(&dir, origin, Some(&dir_name), &host, cx.platform);
                        if let (Some(t), Some(source)) = (found.as_mut(), pin_source) {
                            t.facts.insert("pin".into(), source);
                            t.facts.insert("channel".into(), name.clone());
                        }
                    }
                    None if origin == Origin::Pin => {
                        pinned_missing = Some((name.clone(), pin_source.unwrap_or_default()));
                    }
                    None => {}
                }
            }
        }
    }

    // 3. PATH (a real toolchain, never a rustup proxy) and 4. standard locations.
    if found.is_none() && pinned_missing.is_none() {
        let path_dirs = os::path_dirs(cx.vars);
        for (dirs, origin) in [
            (path_dirs, Origin::Path),
            (standard_bin_dirs(cx.vars, cx.platform), Origin::StandardLocation),
        ] {
            for dir in dirs {
                if !cx.allowed(&dir) || is_rustup_proxy_dir(&dir, cx.platform) {
                    continue;
                }
                let has = |n: &str| os::find_executable(&[n], std::slice::from_ref(&dir), cx.platform);
                if has("cargo").is_some() && has("rustc").is_some() {
                    if let Some(root) = dir.parent() {
                        found = toolchain_at(root, origin, None, &host, cx.platform);
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        searched.push("PATH".into());
        searched.push("standard install locations".into());
    }

    let Some(mut toolchain) = found else {
        return Resolution {
            status: ToolchainStatus::Missing { searched },
            pinned_missing,
        };
    };
    if let Some(home) = &cargo_home {
        toolchain
            .facts
            .insert("cargo_home".into(), home.display().to_string());
    }
    if let Some(home) = &rustup_home {
        toolchain
            .facts
            .insert("rustup_home".into(), home.display().to_string());
    }

    // MSRV of the required projects.
    let layout = cargo_layout(cx.root, cx.files);
    if let (Some((needed, source)), Some(version)) = (&layout.rust_version, &toolchain.version) {
        let req = VersionReq::at_least(needed.clone());
        if !req.matches(version) {
            return Resolution {
                status: ToolchainStatus::TooOld {
                    found: toolchain,
                    needed: req,
                    source: source.clone(),
                },
                pinned_missing: None,
            };
        }
    }
    Resolution {
        status: ToolchainStatus::Found(toolchain),
        pinned_missing: None,
    }
}

/// The Cargo home: a `--env` directory that is a Cargo home, `CARGO_HOME`, `~/.cargo`.
pub fn cargo_home(cx: &DetectContext<'_>) -> Option<PathBuf> {
    cx.env_override
        .filter(|d| is_cargo_home(d))
        .map(Path::to_path_buf)
        .or_else(|| cx.vars.path("CARGO_HOME"))
        .or_else(|| os::home_dir(cx.vars, cx.platform).map(|h| h.join(".cargo")))
        .filter(|h| cx.allowed(h))
}

/// `--env`: a toolchain root (`bin/cargo` + `bin/rustc`) or a Cargo home (`registry/`).
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    let platform = Platform::current();
    is_toolchain_root(path, &platform) || is_cargo_home(path)
}

pub(super) fn is_toolchain_root(dir: &Path, platform: &Platform) -> bool {
    let bin = dir.join("bin");
    bin.join(platform.exe("cargo")).is_file() && bin.join(platform.exe("rustc")).is_file()
}

pub(super) fn is_cargo_home(dir: &Path) -> bool {
    dir.join("registry").is_dir()
}

/// A `bin` directory that holds the rustup proxies (`rustup` next to `cargo`).
pub(super) fn is_rustup_proxy_dir(dir: &Path, platform: &Platform) -> bool {
    dir.join(platform.exe("rustup")).is_file()
}

/// The host triple rustup would use on this platform.
pub fn host_triple(platform: &Platform) -> String {
    let arch = match platform.arch {
        os::Arch::X86_64 => "x86_64",
        os::Arch::Aarch64 => "aarch64",
        os::Arch::Other => platform.arch_name.as_str(),
    };
    match platform.os {
        Os::Windows => format!("{arch}-pc-windows-msvc"),
        Os::MacOs => format!("{arch}-apple-darwin"),
        Os::Linux if platform.musl => format!("{arch}-unknown-linux-musl"),
        Os::Linux => format!("{arch}-unknown-linux-gnu"),
    }
}

/// Toolchain description of the root `dir` (None when `bin/rustc` is missing).
pub(super) fn toolchain_at(
    dir: &Path,
    origin: Origin,
    rustup_name: Option<&str>,
    default_host: &str,
    platform: &Platform,
) -> Option<Toolchain> {
    let bin = dir.join("bin");
    let rustc = bin.join(platform.exe("rustc"));
    let cargo = bin.join(platform.exe("cargo"));
    if !rustc.is_file() {
        return None;
    }
    let mut executables = BTreeMap::new();
    executables.insert("rustc".to_string(), rustc.clone());
    if cargo.is_file() {
        executables.insert("cargo".to_string(), cargo);
    }
    let srv = dir.join("libexec").join(platform.exe("rust-analyzer-proc-macro-srv"));
    if srv.is_file() {
        executables.insert("proc_macro_srv".to_string(), srv);
    }
    let host = rustup_name
        .and_then(triple_suffix)
        .unwrap_or_else(|| default_host.to_string());
    let version = channel_manifest_version(dir).or_else(|| {
        let out = os::toolchain_output(&rustc, &["-vV"])?;
        out.lines()
            .find_map(|l| l.strip_prefix("release:"))
            .and_then(|v| Version::parse(v.trim()))
    });
    let mut facts = BTreeMap::new();
    facts.insert("sysroot".to_string(), dir.display().to_string());
    facts.insert("host".to_string(), host);
    if let Some(name) = rustup_name {
        facts.insert("rustup_toolchain".to_string(), name.to_string());
    }
    let library = rust_src_library(dir);
    if library.join("core").join("src").join("lib.rs").is_file() {
        facts.insert("rust_src".to_string(), library.display().to_string());
    }
    Some(Toolchain {
        id: "rust",
        root: dir.to_path_buf(),
        version,
        executables,
        origin,
        facts,
    })
}

/// `<sysroot>/lib/rustlib/src/rust/library`.
pub(crate) fn rust_src_library(sysroot: &Path) -> PathBuf {
    sysroot
        .join("lib")
        .join("rustlib")
        .join("src")
        .join("rust")
        .join("library")
}

/// `[pkg.rustc] version = "1.92.0 (ded5c06cf 2025-12-08)"` of the channel manifest.
pub(super) fn channel_manifest_version(dir: &Path) -> Option<Version> {
    let manifest = relpath::read_toml(
        &dir.join("lib")
            .join("rustlib")
            .join("multirust-channel-manifest.toml"),
    )?;
    let text = manifest.get("pkg")?.get("rustc")?.get("version")?.as_str()?;
    Version::parse(text.split_whitespace().next()?)
}

/// The host triple at the end of a rustup toolchain name (`stable-x86_64-pc-windows-gnu`).
pub(super) fn triple_suffix(name: &str) -> Option<String> {
    const ARCHES: &[&str] =
        &["x86_64", "aarch64", "i686", "arm", "armv7", "riscv64gc", "powerpc64le", "s390x"];
    let parts: Vec<&str> = name.split('-').collect();
    parts
        .iter()
        .enumerate()
        .skip(1)
        .find(|&(start, arch)| ARCHES.contains(arch) && parts.len() - start >= 3)
        .map(|(start, _)| parts[start..].join("-"))
}

/// `rust-toolchain.toml` / `rust-toolchain` from the root upward: (channel, source).
pub(super) fn pin_file(root: &Path, cx: &DetectContext<'_>) -> Option<(String, String)> {
    let mut dir = Some(root);
    while let Some(d) = dir {
        // Pin files are only read: the repository itself (forbidden for executables) is fine.
        if !cx.allowed(d) && !crate::within(d, root) {
            break;
        }
        for name in ["rust-toolchain.toml", "rust-toolchain"] {
            let path = d.join(name);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let source = if d == root {
                name.to_string()
            } else {
                path.display().to_string()
            };
            let channel = trace_core::formats::toml_value(&text)
                .and_then(|v| v.get("toolchain")?.get("channel")?.as_str().map(str::to_string))
                .or_else(|| {
                    // Legacy format: the whole file is the channel name.
                    (name == "rust-toolchain")
                        .then(|| text.lines().next().unwrap_or_default().trim().to_string())
                        .filter(|c| !c.is_empty() && !c.contains(['=', '[']))
                });
            if let Some(channel) = channel {
                return Some((channel, source));
            }
        }
        dir = d.parent();
    }
    None
}

/// `[overrides]` of rustup's settings: the longest path key that contains `root`.
pub(super) fn directory_override(settings: &Value, root: &Path) -> Option<String> {
    let overrides = settings.get("overrides")?.as_object()?;
    let norm = |p: &str| {
        let s = p.trim_start_matches(r"\\?\").replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    };
    let root_text = norm(&root.display().to_string());
    overrides
        .iter()
        .filter_map(|(k, v)| Some((norm(k), v.as_str()?.to_string())))
        .filter(|(k, _)| root_text == *k || root_text.starts_with(&format!("{k}/")))
        .max_by_key(|(k, _)| k.len())
        .map(|(_, v)| v)
}

/// The installed toolchain directory for `name`: exact, `<name>-<host>`, or the newest
/// `<name>.x-<host>` for a partial version (`1.92`).
pub(super) fn toolchain_dir(toolchains: &Path, name: &str, host: &str) -> Option<(String, PathBuf)> {
    let exact = toolchains.join(name);
    if exact.join("bin").is_dir() {
        return Some((name.to_string(), exact));
    }
    let with_host = format!("{name}-{host}");
    let dir = toolchains.join(&with_host);
    if dir.join("bin").is_dir() {
        return Some((with_host, dir));
    }
    let wanted = Version::parse(name).filter(|_| name.starts_with(|c: char| c.is_ascii_digit()))?;
    let suffix = format!("-{host}");
    subdirs(toolchains)
        .into_iter()
        .filter_map(|(n, p)| {
            let version = Version::parse(n.strip_suffix(&suffix)?)?;
            let prefix_matches = wanted.parts.iter().zip(&version.parts).all(|(a, b)| a == b)
                && version.parts.len() >= wanted.parts.len();
            prefix_matches.then_some((version, n, p))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, n, p)| (n, p))
}

/// `bin` directories of plain (non-rustup) Rust installs per OS.
pub(super) fn standard_bin_dirs(vars: &EnvVars, platform: &Platform) -> Vec<PathBuf> {
    let mut out = Vec::new();
    match platform.os {
        Os::Windows => {
            for key in ["ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(pf) = vars.path(key) {
                    for (_, dir) in os::versioned_children(&pf, "Rust stable MSVC ") {
                        out.push(dir.join("bin"));
                    }
                    for (_, dir) in os::versioned_children(&pf, "Rust stable GNU ") {
                        out.push(dir.join("bin"));
                    }
                }
            }
        }
        Os::Linux => {
            out.push(PathBuf::from("/usr/local/bin"));
            out.push(PathBuf::from("/usr/bin"));
        }
        Os::MacOs => {
            out.push(PathBuf::from("/opt/homebrew/bin"));
            out.push(PathBuf::from("/usr/local/bin"));
        }
    }
    out
}
