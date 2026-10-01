//! .NET SDKs: `global.json` roll-forward selection, installed SDKs and runtimes, workloads, the
//! toolchain and Visual Studio MSBuild.

use super::*;

/// `sdk.rollForward` of global.json (hostfxr policies).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RollForward {
    Patch,
    Feature,
    Minor,
    Major,
    LatestPatch,
    LatestFeature,
    LatestMinor,
    LatestMajor,
    Disable,
}

impl RollForward {
    pub fn parse(text: &str) -> Option<RollForward> {
        Some(match text.trim().to_ascii_lowercase().as_str() {
            "patch" => RollForward::Patch,
            "feature" => RollForward::Feature,
            "minor" => RollForward::Minor,
            "major" => RollForward::Major,
            "latestpatch" => RollForward::LatestPatch,
            "latestfeature" => RollForward::LatestFeature,
            "latestminor" => RollForward::LatestMinor,
            "latestmajor" => RollForward::LatestMajor,
            "disable" => RollForward::Disable,
            _ => return None,
        })
    }
}

/// The `sdk` section of a global.json.
#[derive(Clone, Debug, PartialEq)]
pub struct GlobalJson {
    pub file: PathBuf,
    /// Relative path in the repository ("Src/global.json").
    pub rel: String,
    pub version: Option<Version>,
    pub roll_forward: RollForward,
    pub allow_prerelease: bool,
    /// `sdk.paths` (.NET 10): directories searched for SDKs; "$host$" = the usual search.
    pub paths: Vec<String>,
}

/// Read a global.json (JSON with comments allowed). A file without an `sdk` section pins
/// nothing (None).
pub(crate) fn read_global_json(file: &Path, rel: &str) -> Option<GlobalJson> {
    let value = trace_core::formats::jsonc::parse(&relpath::read_text(file)?)?;
    let sdk = value.get("sdk")?;
    let version = sdk.get("version").and_then(Value::as_str).and_then(Version::parse);
    let roll_forward = sdk
        .get("rollForward")
        .and_then(Value::as_str)
        .and_then(RollForward::parse)
        .unwrap_or(if version.is_some() {
            RollForward::LatestPatch
        } else {
            RollForward::LatestMajor
        });
    let allow_prerelease = sdk.get("allowPrerelease").and_then(Value::as_bool).unwrap_or(true);
    let paths = sdk
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    Some(GlobalJson {
        file: file.to_path_buf(),
        rel: rel.to_string(),
        version,
        roll_forward,
        allow_prerelease,
        paths,
    })
}

/// The global.json nearest to `start` (a relative directory), searching upward to the
/// repository root.
pub(crate) fn find_global_json(root: &Path, start: &str) -> Option<GlobalJson> {
    let mut dir = start.to_string();
    loop {
        let rel = if dir.is_empty() {
            "global.json".to_string()
        } else {
            format!("{dir}/global.json")
        };
        let file = relpath::native(root, &rel);
        if file.is_file() {
            if let Some(g) = read_global_json(&file, &rel) {
                return Some(g);
            }
        }
        if dir.is_empty() {
            return None;
        }
        dir = relpath::parent(&dir).to_string();
    }
}

/// (major, minor, feature band, patch) of an SDK version: 10.0.302 -> (10, 0, 3, 2).
pub(super) fn sdk_key(v: &Version) -> (u64, u64, u64, u64) {
    let part = |i: usize| v.parts.get(i).copied().unwrap_or(0);
    (part(0), part(1), part(2) / 100, part(2) % 100)
}

/// The newest of `versions` in the lowest group (by `group`) - "roll forward to the next
/// higher band and use its latest patch".
pub(super) fn lowest_group_latest<K: Ord>(
    versions: &[&Version],
    group: impl Fn(&Version) -> K,
) -> Option<Version> {
    let lowest = versions.iter().map(|v| group(v)).min()?;
    versions
        .iter()
        .filter(|v| group(v) == lowest)
        .max()
        .map(|v| (*v).clone())
}

/// hostfxr SDK selection over the installed SDK versions (directory names), following
/// global.json: `version` + `rollForward` + `allowPrerelease`. Without a global.json (or
/// without a version) the newest SDK is used.
pub(crate) fn select_sdk(installed: &[Version], global: Option<&GlobalJson>) -> Option<Version> {
    let allow_pre = global.is_none_or(|g| g.allow_prerelease);
    let pool: Vec<&Version> = installed.iter().filter(|v| allow_pre || v.pre.is_none()).collect();
    let Some(req) = global.and_then(|g| g.version.clone()) else {
        return pool.iter().max().map(|v| (*v).clone());
    };
    let policy = global.map_or(RollForward::LatestPatch, |g| g.roll_forward);
    let (rm, rn, rb, rp) = sdk_key(&req);
    let numeric_ge = |v: &Version| {
        let k = sdk_key(v);
        k > (rm, rn, rb, rp) || (k == (rm, rn, rb, rp) && (v.pre.is_none() || v.pre >= req.pre))
    };
    let exact = pool
        .iter()
        .find(|v| sdk_key(v) == (rm, rn, rb, rp) && v.pre == req.pre)
        .map(|v| (*v).clone());
    let same_band: Vec<&Version> = pool
        .iter()
        .copied()
        .filter(|v| {
            let (m, n, b, _) = sdk_key(v);
            (m, n, b) == (rm, rn, rb) && numeric_ge(v)
        })
        .collect();
    let latest_patch = same_band.iter().max().map(|v| (*v).clone());
    let feature = || {
        latest_patch.clone().or_else(|| {
            let higher: Vec<&Version> = pool
                .iter()
                .copied()
                .filter(|v| {
                    let (m, n, b, _) = sdk_key(v);
                    (m, n) == (rm, rn) && b > rb
                })
                .collect();
            lowest_group_latest(&higher, |v| sdk_key(v).2)
        })
    };
    let minor = || {
        feature().or_else(|| {
            let higher: Vec<&Version> = pool
                .iter()
                .copied()
                .filter(|v| {
                    let (m, n, b, _) = sdk_key(v);
                    m == rm && (n, b) > (rn, rb)
                })
                .collect();
            lowest_group_latest(&higher, |v| {
                let (_, n, b, _) = sdk_key(v);
                (n, b)
            })
        })
    };
    let at_least = |filter: &dyn Fn(&Version) -> bool| {
        pool.iter()
            .copied()
            .filter(|v| numeric_ge(v) && filter(v))
            .max()
            .cloned()
    };
    match policy {
        RollForward::Disable => exact,
        RollForward::Patch => exact.or(latest_patch),
        RollForward::LatestPatch => latest_patch,
        RollForward::Feature => feature(),
        RollForward::Minor => minor(),
        RollForward::Major => minor().or_else(|| {
            let higher: Vec<&Version> = pool
                .iter()
                .copied()
                .filter(|v| {
                    let (m, n, b, _) = sdk_key(v);
                    (m, n, b) > (rm, rn, rb)
                })
                .collect();
            lowest_group_latest(&higher, |v| {
                let (m, n, b, _) = sdk_key(v);
                (m, n, b)
            })
        }),
        RollForward::LatestFeature => at_least(&|v| {
            let (m, n, _, _) = sdk_key(v);
            (m, n) == (rm, rn)
        }),
        RollForward::LatestMinor => at_least(&|v| sdk_key(v).0 == rm),
        RollForward::LatestMajor => at_least(&|_| true),
    }
}

/// SDK versions installed in a dotnet root (`<root>/sdk/<version>/dotnet.dll`).
pub(crate) fn sdk_versions(root: &Path) -> Vec<Version> {
    let mut out: Vec<Version> = subdirs(&root.join("sdk"))
        .into_iter()
        .filter(|(_, p)| p.join("dotnet.dll").is_file())
        .filter_map(|(name, _)| Version::parse(&name))
        .collect();
    out.sort();
    out
}

/// A directory accepted as a dotnet root (`--env`): it holds at least one SDK.
pub(super) fn is_dotnet_root(path: &Path) -> bool {
    !sdk_versions(path).is_empty()
}

/// First line of an `/etc/dotnet/install_location[_<arch>]` file.
pub(super) fn install_location_file(path: &Path) -> Option<PathBuf> {
    let text = relpath::read_text(path)?;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let p = PathBuf::from(line);
    p.is_absolute().then_some(p)
}

/// Standard dotnet install roots of the OS (existing or not; filtered by the caller).
pub(super) fn standard_roots(vars: &EnvVars, platform: &Platform) -> Vec<PathBuf> {
    let home = os::home_dir(vars, platform);
    let mut out = Vec::new();
    match platform.os {
        Os::Windows => {
            if let Some(pf) = vars.path("ProgramFiles") {
                out.push(pf.join("dotnet"));
            }
            if platform.arch == Arch::X86_64 {
                if let Some(pf) = vars.path("ProgramFiles(x86)") {
                    out.push(pf.join("dotnet"));
                }
            }
            if let Some(local) = vars.path("LOCALAPPDATA") {
                out.push(local.join("Microsoft").join("dotnet"));
            }
            if let Some(h) = &home {
                out.push(h.join(".dotnet"));
            }
        }
        Os::Linux => {
            let arch = if platform.arch == Arch::Aarch64 {
                "arm64"
            } else {
                "x64"
            };
            for file in [
                format!("/etc/dotnet/install_location_{arch}"),
                "/etc/dotnet/install_location".to_string(),
            ] {
                if let Some(p) = install_location_file(Path::new(&file)) {
                    out.push(p);
                }
            }
            for dir in [
                "/usr/lib/dotnet",
                "/usr/share/dotnet",
                "/usr/lib64/dotnet",
                "/opt/dotnet",
                "/snap/dotnet-sdk/current",
            ] {
                out.push(PathBuf::from(dir));
            }
            if let Some(h) = &home {
                out.push(h.join(".dotnet"));
            }
        }
        Os::MacOs => {
            if let Some(p) = install_location_file(Path::new("/etc/dotnet/install_location")) {
                out.push(p);
            }
            out.push(PathBuf::from("/usr/local/share/dotnet"));
            if platform.arch == Arch::X86_64 {
                out.push(PathBuf::from("/usr/local/share/dotnet/x64"));
            }
            out.push(PathBuf::from("/opt/homebrew/opt/dotnet/libexec"));
            out.push(PathBuf::from("/usr/local/opt/dotnet/libexec"));
            if let Some(h) = &home {
                out.push(h.join(".dotnet"));
            }
        }
    }
    out
}

/// Candidate dotnet roots in lookup order. A global.json `sdk.paths` list without `$host$`
/// and an explicit `--env` override are exclusive (the user's choice always wins).
pub(super) fn candidate_roots(cx: &DetectContext<'_>, global: Option<&GlobalJson>) -> Vec<(PathBuf, Origin)> {
    let mut out: Vec<(PathBuf, Origin)> = Vec::new();
    let mut host = true;
    if let Some(g) = global.filter(|g| !g.paths.is_empty()) {
        host = false;
        let base = g.file.parent().map(Path::to_path_buf).unwrap_or_default();
        for p in &g.paths {
            if p.trim() == "$host$" {
                host = true;
                continue;
            }
            let path = PathBuf::from(p);
            let path = if path.is_absolute() { path } else { base.join(path) };
            out.push((path, Origin::Pin));
        }
    }
    if !host {
        return out;
    }
    if let Some(o) = cx.env_override {
        let root = if o.is_file() {
            o.parent().map(Path::to_path_buf).unwrap_or_default()
        } else {
            o.to_path_buf()
        };
        out.push((root, Origin::Override));
        return out;
    }
    for key in ["DOTNET_ROOT", "DOTNET_ROOT_X64"] {
        if let Some(p) = cx.vars.path(key) {
            out.push((p, Origin::Path));
        }
    }
    if let Some(exe) = lookup::on_path(&["dotnet"], cx.vars, cx.platform) {
        let real = fs::canonicalize(&exe)
            .map(trace_core::inventory::strip_verbatim)
            .unwrap_or(exe);
        if let Some(dir) = real.parent() {
            out.push((dir.to_path_buf(), Origin::Path));
        }
    }
    for p in standard_roots(cx.vars, cx.platform) {
        out.push((p, Origin::StandardLocation));
    }
    let mut seen = BTreeSet::new();
    out.retain(|(p, _)| seen.insert(p.to_string_lossy().to_lowercase()));
    out
}

/// The .NET SDK for this repository (project scan included).
pub fn toolchain(cx: &DetectContext<'_>) -> ToolchainStatus {
    toolchain_for(cx, &project(cx))
}

/// The .NET SDK for an already scanned project: the first dotnet root (lookup order) holding
/// an SDK global.json accepts. `TooOld` when SDKs exist but none is accepted, or when the
/// selected SDK is older than a `netX.Y` target framework.
pub fn toolchain_for(cx: &DetectContext<'_>, project: &DotnetProject) -> ToolchainStatus {
    let global = project.global_json.as_ref();
    let mut searched = Vec::new();
    let mut newest: Option<(Version, PathBuf, Origin)> = None;
    for (root, origin) in candidate_roots(cx, global) {
        searched.push(root.display().to_string());
        if !cx.allowed(&root) {
            continue;
        }
        let sdks = sdk_versions(&root);
        if let Some(top) = sdks.last() {
            if newest.as_ref().is_none_or(|(v, _, _)| top > v) {
                newest = Some((top.clone(), root.clone(), origin));
            }
        }
        let Some(sdk) = select_sdk(&sdks, global) else {
            continue;
        };
        let tc = sdk_toolchain(&root, sdk.clone(), origin, cx.platform, global);
        if let Some((major, source)) = project.highest_net_major() {
            if sdk.parts.first().copied().unwrap_or(0) < major {
                return ToolchainStatus::TooOld {
                    found: tc,
                    needed: VersionReq::at_least(
                        Version::parse(&format!("{major}.0")).unwrap_or_else(|| sdk.clone()),
                    ),
                    source,
                };
            }
        }
        return ToolchainStatus::Found(tc);
    }
    match (newest, global.and_then(|g| g.version.clone().map(|v| (v, g.rel.clone())))) {
        (Some((v, root, origin)), Some((req, source))) => ToolchainStatus::TooOld {
            found: sdk_toolchain(&root, v, origin, cx.platform, global),
            needed: VersionReq::at_least(req),
            source,
        },
        _ => ToolchainStatus::Missing { searched },
    }
}

pub(super) fn sdk_toolchain(
    root: &Path,
    sdk: Version,
    origin: Origin,
    platform: &Platform,
    global: Option<&GlobalJson>,
) -> Toolchain {
    let mut executables = BTreeMap::new();
    executables.insert("dotnet".to_string(), root.join(platform.exe("dotnet")));
    let mut facts = BTreeMap::new();
    facts.insert("sdk_dir".to_string(), root.join("sdk").join(&sdk.text).display().to_string());
    if let Some(g) = global {
        facts.insert("global_json".to_string(), g.rel.clone());
    }
    Toolchain {
        id: "dotnet-sdk",
        root: root.to_path_buf(),
        version: Some(sdk),
        executables,
        origin,
        facts,
    }
}

/// Whether workload `id` is installed for the feature band of `sdk` in `root`
/// (`metadata/workloads/[<arch>/]<band>/InstalledWorkloads/<id>`; MAUI: any `maui*` id).
pub(crate) fn workload_installed(root: &Path, sdk: &Version, id: &str) -> bool {
    let (m, n, b, _) = sdk_key(sdk);
    let band = format!("{m}.{n}.{}", b * 100);
    let workloads = root.join("metadata").join("workloads");
    let mut dirs = vec![workloads.join(&band).join("InstalledWorkloads")];
    for (_, arch) in subdirs(&workloads) {
        dirs.push(arch.join(&band).join("InstalledWorkloads"));
    }
    dirs.iter().any(|d| {
        crate::entries(d).iter().any(|(name, _)| {
            name.eq_ignore_ascii_case(id) || (id == "maui" && name.to_ascii_lowercase().starts_with("maui"))
        })
    })
}

/// Workloads the required projects need that the selected SDK does not have.
pub fn missing_workloads(project: &DotnetProject, toolchain: &Toolchain) -> Vec<String> {
    let Some(sdk) = &toolchain.version else {
        return Vec::new();
    };
    project
        .workloads()
        .into_iter()
        .filter(|id| !workload_installed(&toolchain.root, sdk, id))
        .collect()
}

/// Visual Studio (or Build Tools) MSBuild on Windows, needed by the old project format:
/// `<Program Files>/Microsoft Visual Studio/<version>/<edition>/MSBuild/Current/Bin/MSBuild.exe`.
pub fn visual_studio_msbuild(vars: &EnvVars, platform: &Platform) -> Option<PathBuf> {
    if platform.os != Os::Windows {
        return None;
    }
    for key in ["ProgramFiles", "ProgramFiles(x86)"] {
        let Some(pf) = vars.path(key) else { continue };
        for (_, year) in subdirs(&pf.join("Microsoft Visual Studio")) {
            for (_, edition) in subdirs(&year) {
                let exe = edition
                    .join("MSBuild")
                    .join("Current")
                    .join("Bin")
                    .join("MSBuild.exe");
                if exe.is_file() {
                    return Some(exe);
                }
            }
        }
    }
    None
}

/// `--env` accepts a dotnet root holding an SDK (or the `dotnet` executable in one).
pub(crate) fn accepts_env_path(path: &Path) -> bool {
    if path.is_file() {
        return path.parent().is_some_and(is_dotnet_root);
    }
    is_dotnet_root(path)
}
