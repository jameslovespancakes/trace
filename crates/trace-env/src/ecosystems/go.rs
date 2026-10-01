//! Go: toolchain, module layout (`go.mod` / `go.work`), module cache and dependencies.
//! Read-only; the only execution is `go env GOVERSION` on a Go binary whose root
//! has no `VERSION` file ([`crate::os::toolchain_output`]).
//!
//! * **Toolchain** ([`toolchain`]): candidates in order `--env <GOROOT>`, `GOROOT`, `go` on
//!   PATH (symlinks resolved), standard install locations per OS, toolchains Go downloaded into
//!   the module cache (`golang.org/toolchain@v0.0.1-go1.x.y.<os>-<arch>`). The first candidate
//!   that satisfies the highest `go` directive of the required modules wins (the `toolchain`
//!   directive's version first when installed); none -> `TooOld` naming `go.mod`. Version =
//!   first line of `<GOROOT>/VERSION` (`go1.27.0`). Facts: `GOROOT`, `GOMODCACHE`, `GOPATH`.
//! * **Module cache** ([`module_cache_dir`]): `--env <module cache>` -> `GOMODCACHE` ->
//!   `GOMODCACHE` of the go env file -> first entry of the `GOPATH` list (env, then the go env
//!   file) + `/pkg/mod` -> `~/go/pkg/mod`. The go env file is `$GOENV`, else
//!   `<config dir>/go/env`.
//! * **Layout** ([`go_layout`]): `go.work` at the root (its `use` modules) or the top-most
//!   `go.mod` directories above the Go files; other modules are sub-projects.
//! * **Dependencies** ([`deps`]): every `require` of a required module (after `replace`
//!   directives of `go.mod` and `go.work`) must exist as `<cache>/<escaped path>@<escaped
//!   version>` (a local replacement: its directory). A module with `vendor/modules.txt`
//!   carries its dependencies. `go.sum` is not checked (it lists modules Go never downloads).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use trace_core::Language;

use crate::lookup;
use crate::os::{self, EnvVars, Os, Platform, Version, VersionReq};
use crate::relpath;
use crate::{
    subdirs, DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot, Origin,
    SubProject, Toolchain, ToolchainStatus,
};

/// Dependency hint of the Go ecosystem.
pub const DEPS_HINT: &str = "go mod download";

// ---------------------------------------------------------------------------------------------
// go.mod / go.work (structured reader: lines, quoted tokens, blocks, `//` comments)
// ---------------------------------------------------------------------------------------------

/// A `replace` directive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replace {
    pub old: String,
    pub old_version: Option<String>,
    pub new: String,
    pub new_version: Option<String>,
}

impl Replace {
    /// The replacement is a local directory (`./x`, `../x`, absolute).
    pub fn is_local(&self) -> bool {
        self.new.starts_with("./")
            || self.new.starts_with("../")
            || self.new.starts_with(".\\")
            || self.new.starts_with("..\\")
            || Path::new(&self.new).is_absolute()
    }
}

/// The parts of a `go.mod` / `go.work` trace uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GoMod {
    pub module: String,
    pub go: Option<String>,
    pub toolchain: Option<String>,
    pub requires: Vec<(String, String)>,
    pub replaces: Vec<Replace>,
    /// `use` directives (go.work).
    pub uses: Vec<String>,
}

/// Parse `go.mod` or `go.work` text.
pub(crate) fn parse_go_mod(text: &str) -> GoMod {
    let mut m = GoMod::default();
    let mut block: Option<String> = None;
    for raw in text.lines() {
        let words = words(raw);
        if words.is_empty() {
            continue;
        }
        if let Some(verb) = &block {
            if words[0] == ")" {
                block = None;
                continue;
            }
            apply(&mut m, verb, &words);
            continue;
        }
        if words.len() == 2 && words[1] == "(" {
            block = Some(words[0].clone());
            continue;
        }
        apply(&mut m, &words[0], &words[1..]);
    }
    m
}

fn apply(m: &mut GoMod, verb: &str, args: &[String]) {
    match verb {
        "module" => {
            if let Some(a) = args.first() {
                m.module = a.clone();
            }
        }
        "go" => m.go = args.first().cloned(),
        "toolchain" => m.toolchain = args.first().cloned(),
        "require" => {
            if args.len() >= 2 {
                m.requires.push((args[0].clone(), args[1].clone()));
            }
        }
        "use" => {
            if let Some(a) = args.first() {
                m.uses.push(a.clone());
            }
        }
        "replace" => {
            let Some(arrow) = args.iter().position(|a| a == "=>") else {
                return;
            };
            let (left, right) = (&args[..arrow], &args[arrow + 1..]);
            let (Some(old), Some(new)) = (left.first(), right.first()) else {
                return;
            };
            m.replaces.push(Replace {
                old: old.clone(),
                old_version: left.get(1).cloned(),
                new: new.clone(),
                new_version: right.get(1).cloned(),
            });
        }
        _ => {}
    }
}

/// Tokens of one line: whitespace-separated, `"..."` and `` `...` `` quoted, `//` comments cut.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            break;
        }
        if c == '"' || c == '`' {
            let mut s = String::new();
            i += 1;
            while i < chars.len() && chars[i] != c {
                if c == '"' && chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                s.push(chars[i]);
                i += 1;
            }
            i += 1;
            out.push(s);
            continue;
        }
        let mut s = String::new();
        while i < chars.len() && !chars[i].is_whitespace() {
            if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
                break;
            }
            s.push(chars[i]);
            i += 1;
        }
        out.push(s);
    }
    out
}

/// Module cache escaping: an upper-case letter becomes `!` + the lower-case letter.
pub(crate) fn escape_module(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_ascii_uppercase() {
            out.push('!');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Environment: go env file, GOPATH list, module cache
// ---------------------------------------------------------------------------------------------

/// The user's go env file (`$GOENV`, else `<config dir>/go/env`) as KEY=VALUE pairs.
pub(crate) fn go_env_file(vars: &EnvVars, platform: &Platform) -> BTreeMap<String, String> {
    let path = match vars.get("GOENV").and_then(|v| v.to_str()) {
        Some("off") => return BTreeMap::new(),
        Some(p) if Path::new(p).is_absolute() => PathBuf::from(p),
        _ => match os::config_dir(vars, platform) {
            Some(dir) => dir.join("go").join("env"),
            None => return BTreeMap::new(),
        },
    };
    os::read_key_values(&path)
}

/// First entry of a `GOPATH` list (`;` on Windows, `:` elsewhere), absolute only.
pub(crate) fn gopath_first(list: &str, platform: &Platform) -> Option<PathBuf> {
    list.split(platform.path_list_sep())
        .map(str::trim)
        .find(|s| !s.is_empty())
        .filter(|s| platform.is_absolute(s))
        .map(PathBuf::from)
}

/// The user's GOPATH (first entry): env, then the go env file, then `~/go`.
pub fn gopath(vars: &EnvVars, platform: &Platform, env_file: &BTreeMap<String, String>) -> Option<PathBuf> {
    vars.get("GOPATH")
        .and_then(|v| v.to_str())
        .and_then(|v| gopath_first(v, platform))
        .or_else(|| env_file.get("GOPATH").and_then(|v| gopath_first(v, platform)))
        .or_else(|| os::home_dir(vars, platform).map(|h| h.join("go")))
}

/// The module cache (module docs); `None` when no location can be named.
pub(crate) fn module_cache_dir(cx: &DetectContext<'_>) -> Option<PathBuf> {
    if let Some(dir) = cx.env_override.filter(|d| is_module_cache(d)) {
        return Some(dir.to_path_buf());
    }
    let env_file = go_env_file(cx.vars, cx.platform);
    cx.vars
        .path("GOMODCACHE")
        .or_else(|| {
            env_file
                .get("GOMODCACHE")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        })
        .or_else(|| gopath(cx.vars, cx.platform, &env_file).map(|g| g.join("pkg").join("mod")))
        .filter(|p| cx.allowed(p))
}

fn is_module_cache(dir: &Path) -> bool {
    dir.join("cache").join("download").is_dir()
}

fn is_goroot(dir: &Path, platform: &Platform) -> bool {
    dir.join("bin").join(platform.exe("go")).is_file() && dir.join("src").join("runtime").is_dir()
}

/// `--env`: a GOROOT or a module cache.
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    is_goroot(path, &Platform::current()) || is_module_cache(path)
}

// ---------------------------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------------------------

/// One Go module of the repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GoModule {
    /// Directory relative to the root (`""` = the root).
    pub dir: String,
    pub file: GoMod,
}

/// Required modules, sub-projects and the workspace file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GoLayout {
    pub modules: Vec<GoModule>,
    pub subprojects: Vec<GoModule>,
    /// `go.work` at the root.
    pub work: Option<GoMod>,
}

impl GoLayout {
    /// Highest `go` directive of the required modules / go.work: (version, source file).
    pub fn min_go(&self) -> Option<(Version, String)> {
        let mut best: Option<(Version, String)> = None;
        let mut consider = |text: &Option<String>, source: String| {
            if let Some(v) = text.as_deref().and_then(Version::parse) {
                if best.as_ref().is_none_or(|(b, _)| v > *b) {
                    best = Some((v, source));
                }
            }
        };
        if let Some(w) = &self.work {
            consider(&w.go, "go.work".into());
        }
        for m in &self.modules {
            consider(&m.file.go, relpath::join(&m.dir, "go.mod"));
        }
        best
    }

    /// The `toolchain` directive (go.work first, then the root module).
    pub(crate) fn toolchain_directive(&self) -> Option<Version> {
        self.work
            .as_ref()
            .and_then(|w| w.toolchain.clone())
            .or_else(|| self.modules.iter().find_map(|m| m.file.toolchain.clone()))
            .and_then(|t| Version::parse(&t))
    }
}

/// The Go layout of `root` (module docs).
pub(crate) fn go_layout(root: &Path, files: &[(&str, Language)]) -> GoLayout {
    let mut layout = GoLayout::default();
    let dirs = relpath::manifest_dirs(root, files, &[Language::Go], "go.mod");
    let read = |dir: &str| -> Option<GoModule> {
        let text = std::fs::read_to_string(relpath::under(root, dir).join("go.mod")).ok()?;
        Some(GoModule {
            dir: dir.to_string(),
            file: parse_go_mod(&text),
        })
    };
    if let Ok(text) = std::fs::read_to_string(root.join("go.work")) {
        let work = parse_go_mod(&text);
        let used: BTreeSet<String> = work.uses.iter().filter_map(|u| relpath::normalize("", u)).collect();
        for dir in &used {
            layout.modules.extend(read(dir));
        }
        for dir in &dirs {
            if !used.contains(dir) {
                layout.subprojects.extend(read(dir));
            }
        }
        layout.work = Some(work);
        return layout;
    }
    for dir in &dirs {
        let nested = dirs.iter().any(|other| other != dir && relpath::within(dir, other));
        match read(dir) {
            Some(m) if nested => layout.subprojects.push(m),
            Some(m) => layout.modules.push(m),
            None => {}
        }
    }
    layout
}

// ---------------------------------------------------------------------------------------------
// Toolchain
// ---------------------------------------------------------------------------------------------

/// The Go toolchain for this repository (module docs).
pub fn toolchain(cx: &DetectContext<'_>) -> ToolchainStatus {
    let layout = go_layout(cx.root, cx.files);
    let cache = module_cache_dir(cx);
    let env_file = go_env_file(cx.vars, cx.platform);
    let candidates = candidates(cx, cache.as_deref());
    if candidates.is_empty() {
        return ToolchainStatus::Missing {
            searched: vec![
                "--env".into(),
                "GOROOT".into(),
                "PATH".into(),
                "standard install locations".into(),
            ],
        };
    }
    let described: Vec<Toolchain> = candidates
        .into_iter()
        .filter_map(|(root, origin)| describe(&root, origin, cx, cache.as_deref(), &env_file))
        .collect();
    let Some(first) = described.first().cloned() else {
        return ToolchainStatus::Missing {
            searched: vec!["GOROOT".into(), "PATH".into()],
        };
    };
    let Some((min, source)) = layout.min_go() else {
        return ToolchainStatus::Found(first);
    };
    let req = VersionReq::at_least(min);
    let satisfies = |t: &Toolchain| t.version.as_ref().is_some_and(|v| req.matches(v));
    if let Some(preferred) = layout.toolchain_directive() {
        if let Some(t) = described.iter().find(|t| {
            t.version.as_ref() == Some(&preferred)
                || t.version.as_ref().is_some_and(|v| v.parts == preferred.parts)
        }) {
            if satisfies(t) {
                return ToolchainStatus::Found(t.clone());
            }
        }
    }
    match described.iter().find(|t| satisfies(t)) {
        Some(t) => ToolchainStatus::Found(t.clone()),
        None => ToolchainStatus::TooOld {
            found: first,
            needed: req,
            source,
        },
    }
}

/// GOROOT candidates in search order (existing `bin/go` only, deduplicated).
fn candidates(cx: &DetectContext<'_>, cache: Option<&Path>) -> Vec<(PathBuf, Origin)> {
    let p = cx.platform;
    let mut out: Vec<(PathBuf, Origin)> = Vec::new();
    let push = |root: PathBuf, origin: Origin, out: &mut Vec<(PathBuf, Origin)>| {
        let key = canonical_text(&root);
        if cx.allowed(&root)
            && root.join("bin").join(p.exe("go")).is_file()
            && !out.iter().any(|(r, _)| canonical_text(r) == key)
        {
            out.push((root, origin));
        }
    };
    // An explicit `--env <GOROOT>` is the user's choice: no other candidate.
    if let Some(dir) = cx.env_override.filter(|d| is_goroot(d, p)) {
        push(dir.to_path_buf(), Origin::Override, &mut out);
        return out;
    }
    if let Some(root) = cx.vars.path("GOROOT") {
        push(root, Origin::Path, &mut out);
    }
    if let Some(go) = lookup::on_path(&["go"], cx.vars, p) {
        let real = std::fs::canonicalize(&go).unwrap_or(go);
        if let Some(root) = real.parent().and_then(Path::parent) {
            push(trace_core::inventory::strip_verbatim(root.to_path_buf()), Origin::Path, &mut out);
        }
    }
    for root in standard_roots(cx.vars, p) {
        push(root, Origin::StandardLocation, &mut out);
    }
    if let Some(cache) = cache {
        let toolchains = cache.join("golang.org");
        let mut found: Vec<(Version, PathBuf)> = subdirs(&toolchains)
            .into_iter()
            .filter_map(|(name, dir)| {
                let rest = name.strip_prefix("toolchain@v0.0.1-go")?;
                Some((Version::parse(rest)?, dir))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, dir) in found {
            push(dir, Origin::UserCache, &mut out);
        }
    }
    out
}

/// Comparable text of a directory (canonical when it exists; case-insensitive on Windows).
fn canonical_text(path: &Path) -> String {
    let real = std::fs::canonicalize(path)
        .map(trace_core::inventory::strip_verbatim)
        .unwrap_or_else(|_| path.to_path_buf());
    let text = real.display().to_string().replace('\\', "/");
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// Standard GOROOTs per OS.
fn standard_roots(vars: &EnvVars, p: &Platform) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = os::home_dir(vars, p);
    match p.os {
        Os::Windows => {
            for key in ["ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(pf) = vars.path(key) {
                    out.push(pf.join("Go"));
                }
            }
            if let Some(h) = &home {
                out.push(h.join("scoop").join("apps").join("go").join("current"));
            }
            if let Some(pd) = vars.path("ProgramData") {
                out.push(
                    pd.join("chocolatey")
                        .join("lib")
                        .join("golang")
                        .join("tools")
                        .join("go"),
                );
            }
        }
        Os::Linux => {
            out.push(PathBuf::from("/usr/local/go"));
            out.push(PathBuf::from("/usr/lib/go"));
            for (_, dir) in os::versioned_children(Path::new("/usr/lib"), "go-") {
                out.push(dir);
            }
            out.push(PathBuf::from("/snap/go/current"));
        }
        Os::MacOs => {
            out.push(PathBuf::from("/usr/local/go"));
            out.push(PathBuf::from("/opt/homebrew/opt/go/libexec"));
            out.push(PathBuf::from("/usr/local/opt/go/libexec"));
        }
    }
    if let Some(h) = &home {
        for (_, dir) in os::versioned_children(&h.join("sdk"), "go") {
            out.push(dir);
        }
    }
    // Version managers: mise installs a GOROOT per version, asdf-golang one below `go`.
    out.extend(lookup::mise_installs(vars, p, "go"));
    out.extend(
        lookup::asdf_installs(vars, p, "golang")
            .into_iter()
            .map(|d| d.join("go")),
    );
    out
}

/// Toolchain description of a GOROOT.
fn describe(
    root: &Path,
    origin: Origin,
    cx: &DetectContext<'_>,
    cache: Option<&Path>,
    env_file: &BTreeMap<String, String>,
) -> Option<Toolchain> {
    let go = root.join("bin").join(cx.platform.exe("go"));
    if !go.is_file() {
        return None;
    }
    // "go1.27.0" -> "1.27.0" (the text users know).
    let parse = |line: &str| Version::parse(line.trim().trim_start_matches("go"));
    let version = std::fs::read_to_string(root.join("VERSION"))
        .ok()
        .and_then(|t| t.lines().next().and_then(parse))
        .or_else(|| {
            os::toolchain_output(&go, &["env", "GOVERSION"]).and_then(|t| t.lines().next().and_then(parse))
        });
    let mut executables = BTreeMap::new();
    executables.insert("go".to_string(), go);
    let mut facts = BTreeMap::new();
    facts.insert("GOROOT".to_string(), root.display().to_string());
    if let Some(c) = cache {
        facts.insert("GOMODCACHE".to_string(), c.display().to_string());
    }
    if let Some(g) = gopath(cx.vars, cx.platform, env_file) {
        facts.insert("GOPATH".to_string(), g.display().to_string());
    }
    Some(Toolchain {
        id: "go",
        root: root.to_path_buf(),
        version,
        executables,
        origin,
        facts,
    })
}

// ---------------------------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------------------------

/// Missing `require`d modules of `module` (`path@version`), after `replaces`.
fn missing_requires(
    root: &Path,
    module: &GoModule,
    work_replaces: &[Replace],
    local_modules: &BTreeSet<String>,
    cache: Option<&Path>,
) -> Vec<String> {
    let dir = relpath::under(root, &module.dir);
    if dir.join("vendor").join("modules.txt").is_file() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (path, version) in &module.file.requires {
        if local_modules.contains(path) {
            continue;
        }
        let replace = work_replaces
            .iter()
            .chain(&module.file.replaces)
            .find(|r| r.old == *path && r.old_version.as_ref().is_none_or(|v| v == version));
        let present = match replace {
            Some(r) if r.is_local() => {
                let target = PathBuf::from(&r.new);
                let target = if target.is_absolute() {
                    target
                } else {
                    dir.join(target)
                };
                target.is_dir()
            }
            Some(r) => cache.is_some_and(|c| {
                let v = r.new_version.as_deref().unwrap_or(version);
                c.join(format!("{}@{}", escape_module(&r.new), escape_module(v)))
                    .is_dir()
            }),
            None => cache.is_some_and(|c| {
                c.join(format!("{}@{}", escape_module(path), escape_module(version)))
                    .is_dir()
            }),
        };
        if !present {
            out.push(format!("{path}@{version}"));
        }
    }
    out
}

/// Required modules of the repository that are not in the module cache (module docs).
pub fn deps(cx: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
    let layout = go_layout(cx.root, cx.files);
    let mut report = DepsReport::none_declared();
    report.hint = DEPS_HINT.to_string();
    let cache = toolchain
        .and_then(|t| t.facts.get("GOMODCACHE").map(PathBuf::from))
        .or_else(|| module_cache_dir(cx));
    if let Some(c) = cache.as_ref().filter(|c| c.is_dir()) {
        report.roots.push(LibraryRoot {
            path: c.clone(),
            kind: LibraryKind::Dependency,
            ecosystem: EcosystemId::Go,
            layout: "go_modcache",
            version: None,
        });
    }
    if let Some(t) = toolchain {
        report.roots.push(LibraryRoot {
            path: t.root.join("src"),
            kind: LibraryKind::Stdlib,
            ecosystem: EcosystemId::Go,
            layout: "toolchain_stdlib",
            version: t.version.as_ref().map(|v| v.text.clone()),
        });
    }
    let local: BTreeSet<String> = layout
        .modules
        .iter()
        .chain(&layout.subprojects)
        .map(|m| m.file.module.clone())
        .filter(|m| !m.is_empty())
        .collect();
    let work_replaces: Vec<Replace> = layout.work.as_ref().map(|w| w.replaces.clone()).unwrap_or_default();
    let mut h = blake3::Hasher::new();
    if let Some(c) = &cache {
        h.update(c.display().to_string().as_bytes());
    }
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mut declared = false;
    for m in &layout.modules {
        h.update(m.dir.as_bytes());
        for f in ["go.mod", "go.sum"] {
            h.update(&std::fs::read(relpath::under(cx.root, &m.dir).join(f)).unwrap_or_default());
        }
        declared |= !m.file.requires.is_empty();
        missing.extend(missing_requires(cx.root, m, &work_replaces, &local, cache.as_deref()));
    }
    for m in &layout.subprojects {
        report.subprojects.push(SubProject {
            dir: m.dir.clone(),
            reason: "a separate Go module".into(),
        });
        let sub_missing = missing_requires(cx.root, m, &work_replaces, &local, cache.as_deref());
        if !sub_missing.is_empty() {
            report.notes.push(format!(
                "{}: {} module(s) not installed ({DEPS_HINT})",
                relpath::join(&m.dir, "go.mod"),
                sub_missing.len()
            ));
        }
    }
    h.update(format!("{}", missing.len()).as_bytes());
    report.fingerprint = crate::hex(h);
    report.status = if !missing.is_empty() {
        DepsStatus::Missing
    } else if declared {
        DepsStatus::Installed
    } else {
        DepsStatus::NoneDeclared
    };
    report.missing = missing.into_iter().collect();
    report
}

/// The Go ecosystem ([`crate::Ecosystem`]).
pub struct Go;

impl crate::Ecosystem for Go {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::Go
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
#[path = "../../tests/unit/ecosystems/go.rs"]
mod tests;
