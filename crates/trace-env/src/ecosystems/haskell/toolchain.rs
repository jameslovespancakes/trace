//! GHC, GHCup, cabal / stack and the Haskell language server: pins, GHCup folders, the
//! tools folder and PATH.

use super::*;

/// Detect projects, GHC, cabal / Stack and the cabal directories (module docs).
pub fn detect(cx: &DetectContext<'_>) -> HaskellSetup {
    let (project, pending) = project_layout(cx);
    let ghcup = ghcup_dirs(cx.vars, cx.platform);
    let mut searched: Vec<String> = Vec::new();
    let project_dir = project.as_ref().map(|p| cx.root.join(&p.dir));

    let stack = find_tool("stack", ghcup.as_ref(), cx.vars, cx.platform);
    let cabal = find_tool("cabal", ghcup.as_ref(), cx.vars, cx.platform);
    // The requested GHC.
    let mut wanted: Option<(Version, String)> = None;
    if let (Some(p), Some(dir)) = (&project, &project_dir) {
        let rel = |name: &str| {
            if p.dir.is_empty() {
                name.to_string()
            } else {
                format!("{}/{name}", p.dir)
            }
        };
        match p.tool {
            BuildTool::Cabal => {
                for name in ["cabal.project.local", "cabal.project"] {
                    let fields = relpath::read_small(&dir.join(name), MAX_FILE_BYTES)
                        .map(|t| read_cabal_fields(&t))
                        .unwrap_or_default();
                    if let Some(v) = fields.get("with-compiler").and_then(|w| compiler_version(w)) {
                        wanted = Some((v, rel(name)));
                        break;
                    }
                }
            }
            BuildTool::Stack => {
                let doc = relpath::read_small(&dir.join("stack.yaml"), MAX_FILE_BYTES)
                    .and_then(|t| trace_core::formats::yaml::parse(&t));
                let compiler = doc
                    .as_ref()
                    .and_then(|d| d.get("compiler"))
                    .and_then(Value::as_str)
                    .and_then(compiler_version);
                wanted = compiler
                    .map(|v| (v, rel("stack.yaml")))
                    .or_else(|| stack_work_ghc(dir).map(|v| (v, rel(".stack-work"))));
            }
            BuildTool::Direct => {}
        }
    }
    if wanted.is_none() {
        wanted = ghc_pin(cx.root).map(|v| (v, ".tool-versions".to_string()));
    }
    let (ghc, ghc_version) = match &wanted {
        Some((v, _)) => {
            let text = version_text(v);
            searched.push(format!("GHC {text} (GHCup, PATH, Stack programs)"));
            match ghc_for_version(&text, ghcup.as_ref(), cx.vars, cx.platform) {
                Some(exe) => (Some(exe), Some(v.clone())),
                None => (None, None),
            }
        }
        None => {
            searched.push("PATH (ghc)".into());
            let exe = lookup::on_path(&["ghc"], cx.vars, cx.platform).or_else(|| {
                let g = ghcup.as_ref()?;
                searched.push(g.bin.display().to_string());
                os::find_executable(&["ghc"], std::slice::from_ref(&g.bin), cx.platform).or_else(|| {
                    os::versioned_children(&g.base.join("ghc"), "")
                        .into_iter()
                        .find_map(|(_, d)| os::find_executable(&["ghc"], &[d.join("bin")], cx.platform))
                })
            });
            let version = exe.as_deref().and_then(ghc_version_of);
            (exe, version)
        }
    };
    let ghc = ghc.filter(|g| cx.allowed(g));

    // cabal directories (an `--env` override names the cabal directory or the store).
    let mut cabal_dir: Option<PathBuf> = None;
    let mut cabal_xdg = false;
    let mut store_dir: Option<PathBuf> = None;
    if let Some(o) = cx.env_override {
        if o.join("store").is_dir() {
            cabal_dir = Some(o.to_path_buf());
        } else if looks_like_store(o) {
            store_dir = Some(o.to_path_buf());
        }
    }
    if cabal_dir.is_none() {
        cabal_dir = cx.vars.path("CABAL_DIR");
    }
    let home = os::home_dir(cx.vars, cx.platform);
    if cabal_dir.is_none() {
        match cx.platform.os {
            // cabal >= 3.10 without CABAL_DIR: a legacy single directory (`~/.cabal`, or an
            // older cabal's `%APPDATA%\cabal` holding a store / package lists) when one exists,
            // else the XDG layout mapped to Windows' known folders (config in
            // `%APPDATA%\cabal`, store and package lists in `%LOCALAPPDATA%\cabal`).
            Os::Windows => {
                let legacy = home
                    .as_ref()
                    .map(|h| h.join(".cabal"))
                    .filter(|d| d.is_dir())
                    .or_else(|| {
                        cx.vars
                            .path("APPDATA")
                            .map(|a| a.join("cabal"))
                            .filter(|d| d.join("store").is_dir() || d.join("packages").is_dir())
                    });
                match legacy {
                    Some(d) => cabal_dir = Some(d),
                    None => cabal_xdg = true,
                }
            }
            _ => {
                let dot = home.as_ref().map(|h| h.join(".cabal"));
                if dot.as_ref().is_some_and(|d| d.is_dir()) {
                    cabal_dir = dot;
                } else {
                    cabal_xdg = true;
                }
            }
        }
    }
    let config_file = cx.vars.path("CABAL_CONFIG").or_else(|| {
        if cabal_xdg {
            cx.vars
                .path("XDG_CONFIG_HOME")
                .or_else(|| windows_known(cx, "APPDATA"))
                .or_else(|| home.as_ref().map(|h| h.join(".config")))
                .map(|c| c.join("cabal").join("config"))
        } else {
            cabal_dir.as_ref().map(|d| d.join("config"))
        }
    });
    let config = config_file
        .as_deref()
        .and_then(|p| relpath::read_small(p, MAX_FILE_BYTES))
        .map(|t| read_cabal_fields(&t))
        .unwrap_or_default();
    if store_dir.is_none() {
        store_dir = config
            .get("store-dir")
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| p.is_absolute())
            .or_else(|| {
                if cabal_xdg {
                    cx.vars
                        .path("XDG_STATE_HOME")
                        .or_else(|| windows_known(cx, "LOCALAPPDATA"))
                        .or_else(|| home.as_ref().map(|h| h.join(".local").join("state")))
                        .map(|s| s.join("cabal").join("store"))
                } else {
                    cabal_dir.as_ref().map(|d| d.join("store"))
                }
            });
    }
    let package_cache = config
        .get("remote-repo-cache")
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_absolute())
        .or_else(|| {
            if cabal_xdg {
                cx.vars
                    .path("XDG_CACHE_HOME")
                    .or_else(|| windows_known(cx, "LOCALAPPDATA"))
                    .or_else(|| home.as_ref().map(|h| h.join(".cache")))
                    .map(|c| c.join("cabal").join("packages"))
            } else {
                cabal_dir.as_ref().map(|d| d.join("packages"))
            }
        });
    let msys_dirs = match (&ghcup, cx.platform.os) {
        (Some(g), Os::Windows) => [
            g.base.join("msys64").join("mingw64").join("bin"),
            g.base.join("msys64").join("usr").join("bin"),
        ]
        .into_iter()
        .filter(|d| d.is_dir())
        .collect(),
        _ => Vec::new(),
    };
    HaskellSetup {
        project,
        pending,
        ghcup,
        wanted,
        ghc,
        ghc_version,
        cabal: cabal.filter(|c| cx.allowed(c)),
        stack: stack.filter(|s| cx.allowed(s)),
        cabal_dir,
        cabal_xdg,
        store_dir,
        package_cache,
        msys_dirs,
        searched,
    }
}

/// The toolchain: GHC plus the build tool the project needs.
pub fn toolchain(cx: &DetectContext<'_>) -> ToolchainStatus {
    let setup = detect(cx);
    match haskell_toolchain(&setup) {
        Some(t) => ToolchainStatus::Found(t),
        None => ToolchainStatus::Missing {
            searched: setup.searched,
        },
    }
}

/// The `Toolchain` record (None when GHC or the project's build tool is missing).
pub fn haskell_toolchain(setup: &HaskellSetup) -> Option<Toolchain> {
    let ghc = setup.ghc.clone()?;
    let tool = setup.project.as_ref().map_or(BuildTool::Direct, |p| p.tool);
    match tool {
        BuildTool::Cabal => {
            setup.cabal.as_ref()?;
        }
        BuildTool::Stack => {
            setup.stack.as_ref()?;
        }
        BuildTool::Direct => {}
    }
    let real = ghc_target(&ghc);
    let root = real
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| real.clone());
    let mut executables = BTreeMap::new();
    executables.insert("ghc".to_string(), ghc);
    if let Some(c) = &setup.cabal {
        executables.insert("cabal".to_string(), c.clone());
    }
    if let Some(s) = &setup.stack {
        executables.insert("stack".to_string(), s.clone());
    }
    let mut facts = BTreeMap::new();
    if let Some(g) = &setup.ghcup {
        facts.insert("ghcup".to_string(), g.base.display().to_string());
        if let Some(exe) = os::find_executable(&["ghcup"], std::slice::from_ref(&g.bin), &Platform::current())
        {
            executables.insert("ghcup".to_string(), exe);
        }
    }
    facts.insert("build_tool".to_string(), tool.as_str().to_string());
    if let Some(p) = &setup.project {
        facts.insert("project_dir".to_string(), p.dir.clone());
    }
    if let Some(d) = &setup.cabal_dir {
        facts.insert("cabal_dir".to_string(), d.display().to_string());
    }
    if let Some(d) = &setup.store_dir {
        facts.insert("store_dir".to_string(), d.display().to_string());
    }
    if let Some(v) = &setup.ghc_version {
        facts.insert("compiler_id".to_string(), format!("ghc-{}", version_text(v)));
    }
    let origin = match &setup.wanted {
        Some(_) => Origin::Pin,
        None => Origin::Path,
    };
    Some(Toolchain {
        id: "ghc",
        root,
        version: setup.ghc_version.clone(),
        executables,
        origin,
        facts,
    })
}

/// `9.10.3` (numeric parts only).
pub fn version_text(v: &Version) -> String {
    v.parts.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// `ghc-9.6.7`, `/opt/ghc/bin/ghc-9.6.7`, `ghc-9.6.7.exe` -> 9.6.7 (`ghc` alone: None).
pub(crate) fn compiler_version(text: &str) -> Option<Version> {
    let t = text.trim();
    let name = t.rsplit(['/', '\\']).next().unwrap_or(t);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    let rest = name.strip_prefix("ghc-")?;
    Version::parse(rest)
}

/// `.tool-versions` / mise `ghc` pin.
pub(super) fn ghc_pin(root: &Path) -> Option<Version> {
    if let Some(text) = relpath::read_small(&root.join(".tool-versions"), MAX_FILE_BYTES) {
        for line in text.lines() {
            let mut words = line.split('#').next().unwrap_or_default().split_whitespace();
            if let (Some("ghc"), Some(v)) = (words.next(), words.next()) {
                return Version::parse(v);
            }
        }
    }
    for name in ["mise.toml", ".mise.toml"] {
        let doc = relpath::read_small(&root.join(name), MAX_FILE_BYTES)
            .and_then(|t| trace_core::formats::toml_value(&t));
        if let Some(v) = doc
            .as_ref()
            .and_then(|d| d.get("tools"))
            .and_then(|t| t.get("ghc"))
            .and_then(Value::as_str)
        {
            return Version::parse(v);
        }
    }
    None
}

/// The GHC of a Stack build: `.stack-work/install/<platform>/<hash>/<ghc version>/`.
pub(super) fn stack_work_ghc(project: &Path) -> Option<Version> {
    let install = project.join(".stack-work").join("install");
    let mut found: Vec<Version> = Vec::new();
    for platform in fs::read_dir(&install).ok()?.filter_map(Result::ok) {
        for hash in fs::read_dir(platform.path())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
        {
            for (v, _) in os::versioned_children(&hash.path(), "") {
                found.push(v);
            }
        }
    }
    found.into_iter().max()
}

/// GHCup's existing base directories: the detected toolchain's GHCup, else `<prefix>/ghcup` or
/// `~/.ghcup` (`C:\ghcup` on Windows).
pub fn ghcup_install_dirs(tc: &Toolchain, vars: &EnvVars, platform: &Platform) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = tc.facts.get("ghcup") {
        out.push(PathBuf::from(d));
    }
    if let Some(exe) = tc.executables.get("ghcup") {
        if let Some(d) = super::deps::canonical(exe).parent().and_then(Path::parent) {
            out.push(d.to_path_buf());
        }
    }
    let prefix = vars.path("GHCUP_INSTALL_BASE_PREFIX");
    match platform.os {
        Os::Windows => {
            if let Some(p) = &prefix {
                out.push(p.join("ghcup"));
            }
            out.push(PathBuf::from(r"C:\ghcup"));
        }
        _ => {
            if let Some(p) = prefix.or_else(|| os::home_dir(vars, platform)) {
                out.push(p.join(".ghcup"));
            }
        }
    }
    out.dedup();
    out.into_iter().filter(|d| d.is_dir()).collect()
}

/// GHCup's base and bin directories (`GHCUP_INSTALL_BASE_PREFIX`, XDG mode, defaults).
pub fn ghcup_dirs(vars: &EnvVars, platform: &Platform) -> Option<Ghcup> {
    let home = os::home_dir(vars, platform);
    let prefix = vars.path("GHCUP_INSTALL_BASE_PREFIX");
    let xdg = vars.get("GHCUP_USE_XDG_DIRS").is_some_and(|v| !v.is_empty());
    let candidate = match platform.os {
        Os::Windows => {
            let base = prefix.or_else(|| {
                vars.get("SystemDrive")
                    .and_then(|d| d.to_str())
                    .map(|d| PathBuf::from(format!("{}\\", d.trim_end_matches('\\'))))
            })?;
            let base = base.join("ghcup");
            Ghcup {
                bin: base.join("bin"),
                base,
            }
        }
        _ if xdg => {
            let data = vars
                .path("XDG_DATA_HOME")
                .or_else(|| home.as_ref().map(|h| h.join(".local").join("share")))?;
            let bin = vars
                .path("XDG_BIN_HOME")
                .or_else(|| home.as_ref().map(|h| h.join(".local").join("bin")))?;
            Ghcup {
                base: data.join("ghcup"),
                bin,
            }
        }
        _ => {
            let base = prefix.or(home)?.join(".ghcup");
            Ghcup {
                bin: base.join("bin"),
                base,
            }
        }
    };
    candidate.base.is_dir().then_some(candidate)
}

/// cabal / stack from PATH, GHCup and standard locations.
pub(super) fn find_tool(
    name: &str,
    ghcup: Option<&Ghcup>,
    vars: &EnvVars,
    platform: &Platform,
) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(g) = ghcup {
        dirs.push(g.bin.clone());
    }
    let home = os::home_dir(vars, platform);
    match platform.os {
        Os::Windows => {
            if let Some(a) = vars.path("APPDATA") {
                dirs.push(a.join("local").join("bin"));
                dirs.push(a.join("cabal").join("bin"));
            }
        }
        _ => {
            if let Some(h) = &home {
                dirs.push(h.join(".local").join("bin"));
                dirs.push(h.join(".cabal").join("bin"));
            }
            dirs.push(PathBuf::from("/usr/local/bin"));
            dirs.push(PathBuf::from("/opt/homebrew/bin"));
            dirs.push(PathBuf::from("/usr/bin"));
        }
    }
    Lookup::new(platform, &[])
        .with_path(vars)
        .with(Where::Standard, dirs)
        .find(&[name])
        .map(|(exe, _)| exe)
}

/// The executable of GHC `version` (GHCup, PATH `ghc-<v>` / `ghc`, Stack's programs).
pub(crate) fn ghc_for_version(
    version: &str,
    ghcup: Option<&Ghcup>,
    vars: &EnvVars,
    platform: &Platform,
) -> Option<PathBuf> {
    let versioned = format!("ghc-{version}");
    if let Some(g) = ghcup {
        if let Some(exe) =
            os::find_executable(&["ghc"], &[g.base.join("ghc").join(version).join("bin")], platform)
        {
            return Some(exe);
        }
        if let Some(exe) = os::find_executable(&[versioned.as_str()], std::slice::from_ref(&g.bin), platform)
        {
            return Some(exe);
        }
    }
    if let Some(exe) = lookup::on_path(&[versioned.as_str()], vars, platform) {
        return Some(exe);
    }
    if let Some(exe) = lookup::on_path(&["ghc"], vars, platform) {
        if ghc_version_of(&exe).is_some_and(|v| version_text(&v) == version) {
            return Some(exe);
        }
    }
    let stack_root = vars.path("STACK_ROOT").or_else(|| match platform.os {
        Os::Windows => vars.path("APPDATA").map(|a| a.join("stack")),
        _ => os::home_dir(vars, platform).map(|h| h.join(".stack")),
    });
    let mut program_dirs: Vec<PathBuf> = Vec::new();
    if let Some(r) = stack_root {
        program_dirs.push(r.join("programs"));
    }
    if platform.os == Os::Windows {
        if let Some(l) = vars.path("LOCALAPPDATA") {
            program_dirs.push(l.join("Programs").join("stack"));
        }
    }
    for programs in program_dirs {
        for platform_dir in fs::read_dir(&programs).into_iter().flatten().filter_map(Result::ok) {
            let bin = platform_dir.path().join(&versioned).join("bin");
            if let Some(exe) = os::find_executable(&["ghc"], &[bin], platform) {
                return Some(exe);
            }
        }
    }
    None
}

/// The real GHC behind a GHCup shim (Windows `<name>.shim` `path = ...`) or symlink.
pub(crate) fn ghc_target(exe: &Path) -> PathBuf {
    // `ghc.exe` -> `ghc.shim`; never replace a version suffix (`ghc-9.10.3`) as an extension.
    let shim = if exe.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
        exe.with_extension("shim")
    } else {
        let mut name = exe.file_name().unwrap_or_default().to_os_string();
        name.push(".shim");
        exe.with_file_name(name)
    };
    if shim.is_file() {
        if let Some(target) = os::read_key_values(&shim).get("path").map(PathBuf::from) {
            return target;
        }
    }
    canonical(exe)
}

/// GHC version from files: the GHCup layout `<...>/ghc/<v>/bin/ghc`, a `ghc-<v>` file
/// name, else `ghc --numeric-version` (a toolchain binary, never project code).
pub(crate) fn ghc_version_of(exe: &Path) -> Option<Version> {
    let real = ghc_target(exe);
    let from_layout = real
        .parent()
        .and_then(Path::parent)
        .filter(|d| {
            d.parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n.to_string_lossy() == "ghc")
        })
        .and_then(Path::file_name)
        .and_then(|n| Version::parse(&n.to_string_lossy()));
    from_layout
        .or_else(|| {
            [exe, real.as_path()]
                .iter()
                .find_map(|p| p.file_name().and_then(|n| compiler_version(&n.to_string_lossy())))
        })
        .or_else(|| {
            os::toolchain_output(exe, &["--numeric-version"])
                .and_then(|t| t.lines().next().and_then(Version::parse))
        })
}

/// Directories searched for `haskell-language-server-<ghc>`, in order: `extra` (trace's
/// tools folder), GHCup's `hls/<pinned>`, GHCup's bin, other GHCup HLS versions (newest
/// first), PATH.
pub fn hls_dirs(ghcup: Option<&Ghcup>, pinned: &str, extra: &[PathBuf], vars: &EnvVars) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for d in extra {
        dirs.push(d.join("bin"));
        dirs.push(d.clone());
    }
    if let Some(g) = ghcup {
        let hls = g.base.join("hls");
        dirs.push(hls.join(pinned).join("bin"));
        dirs.push(hls.join(pinned));
        dirs.push(g.bin.clone());
        for (_, d) in os::versioned_children(&hls, "") {
            dirs.push(d.join("bin"));
            dirs.push(d);
        }
    }
    dirs.extend(os::path_dirs(vars));
    let mut unique: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if !unique.contains(&d) {
            unique.push(d);
        }
    }
    unique
}

/// The HLS binary built for exactly GHC `ghc` (`haskell-language-server-9.10.3`).
pub fn find_hls(dirs: &[PathBuf], ghc: &str, platform: &Platform) -> Option<PathBuf> {
    let name = format!("haskell-language-server-{ghc}");
    os::find_executable(&[name.as_str()], dirs, platform)
}

/// The HLS release of a binary, from its GHCup location (`hls/<v>/...`,
/// `lib/haskell-language-server-<v>/...`).
pub fn hls_version_of(exe: &Path) -> Option<String> {
    let real = ghc_target(exe);
    let parts: Vec<String> = real
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    for (i, p) in parts.iter().enumerate() {
        if p == "hls" {
            if let Some(v) = parts.get(i + 1).filter(|v| Version::parse(v).is_some()) {
                return Some(v.clone());
            }
        }
        if let Some(v) = p.strip_prefix("haskell-language-server-") {
            if parts.get(i + 1).is_some_and(|n| n == "bin")
                && Version::parse(v).is_some()
                && v.split('.').count() >= 4
            {
                return Some(v.to_string());
            }
        }
    }
    None
}
