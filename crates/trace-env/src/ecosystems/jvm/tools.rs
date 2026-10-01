//! Build tool locations: the Maven local repository, Maven / Gradle installations and
//! wrappers, sbt / Mill / Coursier launchers and caches, the Android SDK.

use super::*;

/// `-Dkey=value` definitions of `.mvn/maven.config` (user properties of every Maven run).
pub(super) fn maven_config_props(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(text) = relpath::read_small(&root.join(".mvn").join("maven.config"), MAX_FILE_BYTES) {
        define_tokens(text.split_whitespace(), &mut out);
    }
    out
}

pub(super) fn define_tokens<'a>(tokens: impl Iterator<Item = &'a str>, out: &mut BTreeMap<String, String>) {
    let mut pending_d = false;
    for token in tokens {
        let def = if pending_d {
            pending_d = false;
            Some(token)
        } else if token == "-D" {
            pending_d = true;
            None
        } else {
            token.strip_prefix("-D")
        };
        if let Some((k, v)) = def.and_then(|d| d.split_once('=')) {
            out.insert(k.to_string(), v.trim_matches('"').to_string());
        }
    }
}

/// The Maven local repository: `maven.repo.local` (maven.config, MAVEN_OPTS), the user or
/// global `settings.xml` `<localRepository>`, else `~/.m2/repository`.
pub(super) fn maven_repo(
    cx: &DetectContext<'_>,
    home: Option<&Path>,
    config: &BTreeMap<String, String>,
) -> PathBuf {
    let absolute = |text: &str| -> PathBuf {
        let p = PathBuf::from(text);
        if p.is_absolute() {
            p
        } else {
            cx.root.join(p)
        }
    };
    if let Some(v) = config.get("maven.repo.local") {
        return absolute(v);
    }
    if let Some(opts) = cx.vars.get("MAVEN_OPTS").and_then(|v| v.to_str()) {
        let mut defs = BTreeMap::new();
        define_tokens(opts.split_whitespace(), &mut defs);
        if let Some(v) = defs.get("maven.repo.local") {
            return absolute(v);
        }
    }
    let mut settings = Vec::new();
    if let Some(h) = home {
        settings.push(h.join(".m2").join("settings.xml"));
    }
    for key in ["MAVEN_HOME", "M2_HOME"] {
        if let Some(m) = cx.vars.path(key) {
            settings.push(m.join("conf").join("settings.xml"));
        }
    }
    for file in settings {
        let Some(root) = relpath::read_small(&file, MAX_FILE_BYTES).and_then(|t| xml::parse(&t)) else {
            continue;
        };
        if let Some(local) = root.text_at(&["localRepository"]).filter(|t| !t.is_empty()) {
            let mut props = BTreeMap::new();
            if let Some(h) = home {
                props.insert("user.home".to_string(), h.display().to_string());
            }
            if let Some(v) = interpolate(local, &props, cx.vars) {
                return absolute(&v);
            }
        }
    }
    home.map(|h| h.join(".m2").join("repository")).unwrap_or_default()
}

/// sbt's boot folder: the launcher of sbt 1.10+ keeps it in the OS cache folder
/// (`%LOCALAPPDATA%\sbt\boot`, `$XDG_CACHE_HOME/sbt/boot`, `~/Library/Caches/sbt/boot`), older
/// launchers in `~/.sbt/boot`. The first existing one wins; none -> the current default.
pub(super) fn sbt_boot_dir(vars: &EnvVars, p: &Platform, home: Option<&Path>) -> Option<PathBuf> {
    let cache_default = match p.os {
        Os::Windows => vars.path("LOCALAPPDATA").map(|d| d.join("sbt").join("boot")),
        Os::Linux => os::cache_dir(vars, p).map(|d| d.join("sbt").join("boot")),
        Os::MacOs => home.map(|h| h.join("Library").join("Caches").join("sbt").join("boot")),
    };
    let legacy = home.map(|h| h.join(".sbt").join("boot"));
    let candidates: Vec<PathBuf> = cache_default.into_iter().chain(legacy).collect();
    candidates
        .iter()
        .find(|d| d.is_dir())
        .or_else(|| candidates.first())
        .cloned()
}

pub(super) fn default_coursier_cache(vars: &EnvVars, p: &Platform) -> Option<PathBuf> {
    match p.os {
        Os::Windows => vars
            .path("LOCALAPPDATA")
            .map(|d| d.join("Coursier").join("cache").join("v1")),
        Os::Linux => os::cache_dir(vars, p).map(|d| d.join("coursier").join("v1")),
        Os::MacOs => {
            os::home_dir(vars, p).map(|h| h.join("Library").join("Caches").join("Coursier").join("v1"))
        }
    }
}

/// Coursier's application launchers (`cs install`).
pub(super) fn coursier_bin_dirs(vars: &EnvVars, p: &Platform, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = os::path_dirs(vars);
    match p.os {
        Os::Windows => {
            if let Some(d) = vars.path("LOCALAPPDATA") {
                dirs.push(d.join("Coursier").join("data").join("bin"));
            }
        }
        Os::Linux => {
            if let Some(d) = os::data_local_dir(vars, p) {
                dirs.push(d.join("coursier").join("bin"));
            }
        }
        Os::MacOs => {
            if let Some(h) = home {
                dirs.push(
                    h.join("Library")
                        .join("Application Support")
                        .join("Coursier")
                        .join("bin"),
                );
            }
        }
    }
    dirs
}

pub(super) fn sbt_names(p: &Platform) -> Vec<&'static str> {
    if p.os == Os::Windows {
        vec!["sbt.bat", "sbt.cmd", "sbt"]
    } else {
        vec!["sbt"]
    }
}

pub(super) fn sbt_dirs(vars: &EnvVars, p: &Platform, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = coursier_bin_dirs(vars, p, home);
    if let Some(h) = home {
        dirs.push(
            h.join(".sdkman")
                .join("candidates")
                .join("sbt")
                .join("current")
                .join("bin"),
        );
    }
    match p.os {
        Os::Windows => {
            dirs.push(PathBuf::from(r"C:\Program Files (x86)\sbt\bin"));
            dirs.push(PathBuf::from(r"C:\Program Files\sbt\bin"));
        }
        Os::Linux => {
            dirs.push(PathBuf::from("/usr/bin"));
            dirs.push(PathBuf::from("/usr/local/bin"));
            dirs.push(PathBuf::from("/usr/share/sbt/bin"));
        }
        Os::MacOs => {
            dirs.push(PathBuf::from("/opt/homebrew/bin"));
            dirs.push(PathBuf::from("/usr/local/bin"));
        }
    }
    dirs
}

/// A launcher script / binary found by name in `dirs` (sbt, Mill, Scala CLI).
pub(super) fn launcher_tool(
    cx: &DetectContext<'_>,
    id: &'static str,
    names: &[&str],
    dirs: &[PathBuf],
) -> Option<Toolchain> {
    let dirs: Vec<PathBuf> = dirs.iter().filter(|d| cx.allowed(d)).cloned().collect();
    let exe = os::find_executable(names, &dirs, cx.platform)?;
    let root = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut executables = BTreeMap::new();
    executables.insert(id.to_string(), exe);
    Some(Toolchain {
        id,
        root,
        version: None,
        executables,
        origin: Origin::Path,
        facts: BTreeMap::new(),
    })
}

/// `lib/<prefix><version>.jar` of a distribution.
pub(super) fn lib_jar_version(dist: &Path, prefixes: &[&str]) -> Option<Version> {
    entries(&dist.join("lib")).into_iter().find_map(|(name, _)| {
        let stem = name.strip_suffix(".jar")?;
        prefixes
            .iter()
            .find_map(|prefix| stem.strip_prefix(*prefix))
            .and_then(Version::parse)
    })
}

pub(super) fn maven_wrapper_version(root: &Path) -> Option<String> {
    let props = os::read_key_values(&root.join(".mvn").join("wrapper").join("maven-wrapper.properties"));
    let url = unescape_properties(props.get("distributionUrl")?);
    let file = url.rsplit('/').next()?;
    let rest = file.strip_prefix("apache-maven-")?;
    let version = rest
        .strip_suffix("-bin.zip")
        .or_else(|| rest.strip_suffix("-bin.tar.gz"))
        .or_else(|| rest.strip_suffix(".zip"))?;
    Some(version.to_string())
}

pub(super) fn maven_tool(
    cx: &DetectContext<'_>,
    home: Option<&Path>,
    wrapper: Option<&str>,
) -> Option<Toolchain> {
    let p = cx.platform;
    let is_home = |d: &Path| lib_jar_version(d, &["maven-core-"]);
    let found = |root: PathBuf, version: Version, origin: Origin| {
        let mut executables = BTreeMap::new();
        for name in ["mvn", "mvn.cmd"] {
            let exe = root.join("bin").join(name);
            if exe.is_file() {
                executables.insert("mvn".to_string(), exe);
                break;
            }
        }
        Some(Toolchain {
            id: "maven",
            root,
            version: Some(version),
            executables,
            origin,
            facts: BTreeMap::new(),
        })
    };
    if let (Some(v), Some(h)) = (wrapper, home) {
        let dists = h.join(".m2").join("wrapper").join("dists");
        for parent in [
            dists.join(format!("apache-maven-{v}-bin")),
            dists.join(format!("apache-maven-{v}")),
        ] {
            for (_, hash_dir) in entries(&parent) {
                for candidate in [hash_dir.join(format!("apache-maven-{v}")), hash_dir.clone()] {
                    if let Some(version) = is_home(&candidate) {
                        return found(candidate, version, Origin::Project);
                    }
                }
            }
        }
    }
    for key in ["MAVEN_HOME", "M2_HOME"] {
        if let Some(dir) = cx.vars.path(key).filter(|d| cx.allowed(d)) {
            if let Some(version) = is_home(&dir) {
                return found(dir, version, Origin::Path);
            }
        }
    }
    if let Some(mvn) = lookup::on_path(&["mvn", "mvn.cmd"], cx.vars, p) {
        let real = fs::canonicalize(&mvn).unwrap_or(mvn);
        if let Some(dir) = real.parent().and_then(Path::parent) {
            if let Some(version) = is_home(dir) {
                return found(dir.to_path_buf(), version, Origin::Path);
            }
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    match p.os {
        Os::Windows => {
            for (_, d) in entries(Path::new(r"C:\Program Files\Apache")) {
                candidates.push(d);
            }
            for (_, d) in entries(Path::new(r"C:\ProgramData\chocolatey\lib\maven")) {
                candidates.push(d);
            }
        }
        Os::Linux => {
            candidates.push(PathBuf::from("/usr/share/maven"));
            candidates.push(PathBuf::from("/opt/maven"));
        }
        Os::MacOs => {
            candidates.push(PathBuf::from("/opt/homebrew/opt/maven/libexec"));
            candidates.push(PathBuf::from("/usr/local/opt/maven/libexec"));
        }
    }
    if let Some(h) = home {
        candidates.push(h.join(".sdkman").join("candidates").join("maven").join("current"));
    }
    candidates.extend(lookup::mise_installs(cx.vars, p, "maven"));
    candidates.extend(lookup::asdf_installs(cx.vars, p, "maven"));
    candidates
        .into_iter()
        .filter(|d| cx.allowed(d))
        .find_map(|d| is_home(&d).map(|v| (d, v)))
        .and_then(|(d, v)| found(d, v, Origin::StandardLocation))
}

/// Gradle: the installed wrapper distribution of the first wrapper that has one, else an
/// installed Gradle (GRADLE_HOME, PATH, standard folders).
pub(super) fn gradle_tool(
    cx: &DetectContext<'_>,
    home: Option<&Path>,
    wrappers: &[GradleWrapper],
) -> Option<Toolchain> {
    let p = cx.platform;
    let is_home = |d: &Path| lib_jar_version(d, &["gradle-core-api-", "gradle-launcher-"]);
    let found = |root: PathBuf, version: Version, origin: Origin, wrapper: bool| {
        let mut executables = BTreeMap::new();
        for name in ["gradle", "gradle.bat"] {
            let exe = root.join("bin").join(name);
            if exe.is_file() {
                executables.insert("gradle".to_string(), exe);
                break;
            }
        }
        let mut facts = BTreeMap::new();
        facts.insert("wrapper".to_string(), wrapper.to_string());
        Some(Toolchain {
            id: "gradle",
            root,
            version: Some(version),
            executables,
            origin,
            facts,
        })
    };
    for w in wrappers {
        if let Some(dir) = &w.installed {
            let version = is_home(dir).or_else(|| Version::parse(&w.version))?;
            return found(dir.clone(), version, Origin::UserCache, true);
        }
    }
    if let Some(dir) = cx.vars.path("GRADLE_HOME").filter(|d| cx.allowed(d)) {
        if let Some(version) = is_home(&dir) {
            return found(dir, version, Origin::Path, false);
        }
    }
    if let Some(exe) = lookup::on_path(&["gradle", "gradle.bat"], cx.vars, p) {
        let real = fs::canonicalize(&exe).unwrap_or(exe);
        if let Some(dir) = real.parent().and_then(Path::parent) {
            if let Some(version) = is_home(dir) {
                return found(dir.to_path_buf(), version, Origin::Path, false);
            }
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        candidates.push(h.join(".sdkman").join("candidates").join("gradle").join("current"));
    }
    candidates.extend(lookup::mise_installs(cx.vars, p, "gradle"));
    candidates.extend(lookup::asdf_installs(cx.vars, p, "gradle"));
    match p.os {
        Os::Windows => {
            for (_, d) in entries(Path::new(r"C:\Gradle")) {
                candidates.push(d);
            }
            for (_, d) in entries(Path::new(r"C:\ProgramData\chocolatey\lib\gradle\tools")) {
                candidates.push(d);
            }
        }
        Os::Linux => {
            for (_, d) in entries(Path::new("/opt/gradle")) {
                candidates.push(d);
            }
            candidates.push(PathBuf::from("/usr/share/gradle"));
        }
        Os::MacOs => {
            candidates.push(PathBuf::from("/opt/homebrew/opt/gradle/libexec"));
            candidates.push(PathBuf::from("/usr/local/opt/gradle/libexec"));
        }
    }
    candidates
        .into_iter()
        .filter(|d| cx.allowed(d))
        .find_map(|d| is_home(&d).map(|v| (d, v)))
        .and_then(|(d, v)| found(d, v, Origin::StandardLocation, false))
}

/// The Android SDK: `ANDROID_HOME`, `ANDROID_SDK_ROOT`, `local.properties` `sdk.dir`, then the
/// standard folder of the OS. A folder counts when it has `platforms/` or `platform-tools/`.
pub(super) fn android_sdk(cx: &DetectContext<'_>, home: Option<&Path>) -> Option<PathBuf> {
    let valid =
        |d: &Path| cx.allowed(d) && (d.join("platforms").is_dir() || d.join("platform-tools").is_dir());
    let mut candidates: Vec<PathBuf> = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(|k| cx.vars.path(k))
        .collect();
    if let Some(dir) = os::read_key_values(&cx.root.join("local.properties")).get("sdk.dir") {
        candidates.push(PathBuf::from(unescape_properties(dir)));
    }
    match cx.platform.os {
        Os::Windows => {
            if let Some(d) = cx.vars.path("LOCALAPPDATA") {
                candidates.push(d.join("Android").join("Sdk"));
            }
        }
        Os::MacOs => {
            if let Some(h) = home {
                candidates.push(h.join("Library").join("Android").join("sdk"));
            }
        }
        Os::Linux => {
            if let Some(h) = home {
                candidates.push(h.join("Android").join("Sdk"));
            }
        }
    }
    candidates.into_iter().find(|d| valid(d))
}

/// Java properties escapes (`\:`, `\=`, `\\`).
pub(super) fn unescape_properties(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}
