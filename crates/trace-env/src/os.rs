//! OS abstraction: the current platform, a snapshot of the process
//! environment, per-OS standard directories, executable lookup, version parsing and the one
//! allowed kind of execution (a toolchain binary asked for its version).
//!
//! Everything here is dynamic (`std::env::consts`, environment variables, the file system);
//! nothing is hard-wired to one machine. Tests build their own [`EnvVars`] / [`Platform`].

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Os {
    Windows,
    Linux,
    MacOs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Arch {
    X86_64,
    Aarch64,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
    /// `std::env::consts::ARCH` (e.g. "x86_64", "aarch64", "riscv64").
    pub arch_name: String,
    /// Linux with the musl C library (`/lib/ld-musl-*.so.1` exists).
    pub musl: bool,
}

impl Platform {
    /// The platform this process runs on (`std::env::consts` + a musl loader probe on Linux).
    pub fn current() -> Platform {
        let os = match std::env::consts::OS {
            "windows" => Os::Windows,
            "macos" => Os::MacOs,
            _ => Os::Linux,
        };
        let arch_name = std::env::consts::ARCH.to_string();
        let arch = match arch_name.as_str() {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            _ => Arch::Other,
        };
        let musl = os == Os::Linux
            && fs::read_dir("/lib")
                .map(|rd| {
                    rd.filter_map(Result::ok).any(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with("ld-musl-") && name.ends_with(".so.1")
                    })
                })
                .unwrap_or(false);
        Platform {
            os,
            arch,
            arch_name,
            musl,
        }
    }

    /// Install-record key: "windows-x86_64", "linux-aarch64", "macos-aarch64" (+ "-musl").
    pub fn key(&self) -> String {
        let os = match self.os {
            Os::Windows => "windows",
            Os::Linux => "linux",
            Os::MacOs => "macos",
        };
        let arch = match self.arch {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
            Arch::Other => self.arch_name.as_str(),
        };
        let musl = if self.musl { "-musl" } else { "" };
        format!("{os}-{arch}{musl}")
    }

    /// "Windows" | "Linux" | "macOS".
    pub fn os_name(&self) -> &'static str {
        match self.os {
            Os::Windows => "Windows",
            Os::Linux => "Linux",
            Os::MacOs => "macOS",
        }
    }

    /// "Linux on aarch64".
    pub fn display(&self) -> String {
        format!("{} on {}", self.os_name(), self.arch_name)
    }

    /// Executable file name: `name` + ".exe" on Windows (unless it already has an extension
    /// such as `.exe`, `.cmd` or `.bat`).
    pub fn exe(&self, name: &str) -> String {
        if self.os != Os::Windows {
            return name.to_string();
        }
        let lower = name.to_ascii_lowercase();
        if [".exe", ".cmd", ".bat", ".com", ".ps1"]
            .iter()
            .any(|ext| lower.ends_with(ext))
        {
            name.to_string()
        } else {
            format!("{name}.exe")
        }
    }

    /// Whether `path` is absolute on this platform (not the running one): a drive or UNC
    /// path on Windows, a `/` path elsewhere.
    pub fn is_absolute(&self, path: &str) -> bool {
        if self.os == Os::Windows {
            let b = path.as_bytes();
            (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
                || path.starts_with("\\\\")
                || path.starts_with("//")
        } else {
            path.starts_with('/')
        }
    }

    /// Separator of PATH-like lists: ';' on Windows, ':' elsewhere.
    pub fn path_list_sep(&self) -> char {
        if self.os == Os::Windows {
            ';'
        } else {
            ':'
        }
    }

    /// The GNU C library version of this Linux (None elsewhere, on musl, or when no libc is
    /// found). Portable Node 24 needs glibc 2.28 or newer.
    pub fn glibc(&self) -> Option<Version> {
        if self.os != Os::Linux || self.musl {
            return None;
        }
        glibc_version()
    }

    /// The Linux distribution key used by binary package snapshots ("noble", "jammy",
    /// "bookworm", "rhel9", "opensuse156"); None elsewhere or when `/etc/os-release` is
    /// missing.
    pub fn linux_distro(&self) -> Option<String> {
        if self.os != Os::Linux {
            return None;
        }
        distro_from_os_release(&read_key_values(Path::new("/etc/os-release")))
            .or_else(|| distro_from_os_release(&read_key_values(Path::new("/usr/lib/os-release"))))
    }
}

/// Where the dynamic C library lives on common Linux layouts (Debian multiarch, Fedora/RHEL
/// lib64, Arch/Alpine lib).
const LIBC_CANDIDATES: &[&str] = &[
    "/lib/x86_64-linux-gnu/libc.so.6",
    "/lib/aarch64-linux-gnu/libc.so.6",
    "/usr/lib/x86_64-linux-gnu/libc.so.6",
    "/usr/lib/aarch64-linux-gnu/libc.so.6",
    "/lib64/libc.so.6",
    "/usr/lib64/libc.so.6",
    "/lib/libc.so.6",
    "/usr/lib/libc.so.6",
];

/// The glibc version read from the library's ELF symbol-version definitions (`GLIBC_2.x`);
/// the highest defined version is the library's version. Read-only: nothing is executed.
pub(crate) fn glibc_version() -> Option<Version> {
    LIBC_CANDIDATES.iter().find_map(|p| {
        let bytes = fs::read(p).ok()?;
        glibc_from_bytes(&bytes)
    })
}

/// Highest `GLIBC_<major>.<minor>[.<patch>]` version name in `bytes` (the version-definition
/// strings of a `libc.so.6`). `GLIBC_PRIVATE` and other non-numeric names are ignored.
pub(crate) fn glibc_from_bytes(bytes: &[u8]) -> Option<Version> {
    const TAG: &[u8] = b"GLIBC_";
    let mut best: Option<Version> = None;
    let mut i = 0;
    while i + TAG.len() < bytes.len() {
        if &bytes[i..i + TAG.len()] != TAG {
            i += 1;
            continue;
        }
        let start = i + TAG.len();
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            end += 1;
        }
        // A version name is NUL-terminated in the string table.
        let terminated = bytes.get(end) == Some(&0);
        if terminated && end > start && bytes[start].is_ascii_digit() {
            if let Some(v) = std::str::from_utf8(&bytes[start..end]).ok().and_then(Version::parse) {
                if best.as_ref().is_none_or(|b| v > *b) {
                    best = Some(v);
                }
            }
        }
        i = end.max(i + 1);
    }
    best
}

/// Distribution key from parsed `os-release` fields: `VERSION_CODENAME` (Ubuntu, Debian);
/// RHEL and its rebuilds -> `rhel<major>`; openSUSE Leap -> `opensuse<major><minor>`.
pub(crate) fn distro_from_os_release(fields: &BTreeMap<String, String>) -> Option<String> {
    let id = fields.get("ID").map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let like = fields
        .get("ID_LIKE")
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let version = fields.get("VERSION_ID").cloned().unwrap_or_default();
    let rhel_family = ["rhel", "centos", "rocky", "almalinux", "ol"].contains(&id.as_str())
        || (like.split_whitespace().any(|w| w == "rhel") && id != "fedora");
    if rhel_family {
        let major = version.split('.').next().unwrap_or_default();
        return (!major.is_empty()).then(|| format!("rhel{major}"));
    }
    if id == "opensuse-leap" {
        let digits: String = version.chars().filter(char::is_ascii_digit).collect();
        return (!digits.is_empty()).then(|| format!("opensuse{digits}"));
    }
    fields
        .get("VERSION_CODENAME")
        .filter(|c| !c.is_empty())
        .map(|c| c.to_ascii_lowercase())
}

/// Snapshot of the process environment (tests build their own). Keys are case-insensitive
/// on Windows (stored upper-case there).
#[derive(Clone, Debug, Default)]
pub struct EnvVars {
    vars: BTreeMap<String, OsString>,
}

fn env_key(key: &str) -> String {
    if cfg!(windows) {
        key.to_ascii_uppercase()
    } else {
        key.to_string()
    }
}

impl EnvVars {
    pub fn from_process() -> EnvVars {
        let mut vars = BTreeMap::new();
        for (k, v) in trace_core::env::vars() {
            vars.insert(env_key(&k), v);
        }
        EnvVars { vars }
    }

    pub fn from_pairs(pairs: &[(&str, &str)]) -> EnvVars {
        EnvVars {
            vars: pairs.iter().map(|(k, v)| (env_key(k), OsString::from(v))).collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.vars.get(&env_key(key)).map(OsString::as_os_str)
    }

    /// Set (or replace) a variable (tests and derived environments).
    pub fn set(&mut self, key: &str, value: impl Into<OsString>) {
        self.vars.insert(env_key(key), value.into());
    }

    /// The variable as a path: non-empty and absolute only.
    pub fn path(&self, key: &str) -> Option<PathBuf> {
        self.get(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    }
}

/// The user's home directory (`USERPROFILE` on Windows, `HOME` elsewhere).
pub fn home_dir(vars: &EnvVars, p: &Platform) -> Option<PathBuf> {
    match p.os {
        Os::Windows => vars.path("USERPROFILE").or_else(|| vars.path("HOME")),
        _ => vars.path("HOME"),
    }
}

/// `%LOCALAPPDATA%` | `$XDG_DATA_HOME` or `~/.local/share` | `~/Library/Application Support`.
pub fn data_local_dir(vars: &EnvVars, p: &Platform) -> Option<PathBuf> {
    match p.os {
        Os::Windows => vars.path("LOCALAPPDATA"),
        Os::Linux => vars
            .path("XDG_DATA_HOME")
            .or_else(|| home_dir(vars, p).map(|h| h.join(".local").join("share"))),
        Os::MacOs => home_dir(vars, p).map(|h| h.join("Library").join("Application Support")),
    }
}

/// `%APPDATA%` | `$XDG_CONFIG_HOME` or `~/.config` | `~/Library/Application Support`.
pub(crate) fn config_dir(vars: &EnvVars, p: &Platform) -> Option<PathBuf> {
    match p.os {
        Os::Windows => vars.path("APPDATA"),
        Os::Linux => vars
            .path("XDG_CONFIG_HOME")
            .or_else(|| home_dir(vars, p).map(|h| h.join(".config"))),
        Os::MacOs => home_dir(vars, p).map(|h| h.join("Library").join("Application Support")),
    }
}

/// `%LOCALAPPDATA%` | `$XDG_CACHE_HOME` or `~/.cache` | `~/Library/Caches`.
pub fn cache_dir(vars: &EnvVars, p: &Platform) -> Option<PathBuf> {
    match p.os {
        Os::Windows => vars.path("LOCALAPPDATA"),
        Os::Linux => vars
            .path("XDG_CACHE_HOME")
            .or_else(|| home_dir(vars, p).map(|h| h.join(".cache"))),
        Os::MacOs => home_dir(vars, p).map(|h| h.join("Library").join("Caches")),
    }
}

/// Absolute PATH entries in order.
pub fn path_dirs(vars: &EnvVars) -> Vec<PathBuf> {
    vars.get("PATH")
        .map(|v| std::env::split_paths(v).filter(|p| p.is_absolute()).collect())
        .unwrap_or_default()
}

/// The directories a shell script's commands are found in on this machine, in order, each
/// once: PATH, then (Windows) the POSIX layouts of Git for Windows / MSYS (`usr/bin`, `bin`,
/// `mingw64/bin` under the root of a `bash` / `git` on PATH or `<ProgramFiles>/Git`).
pub fn shell_program_dirs(vars: &EnvVars, p: &Platform) -> Vec<PathBuf> {
    let path = path_dirs(vars);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let push = |d: PathBuf, dirs: &mut Vec<PathBuf>| {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    for d in &path {
        push(d.clone(), &mut dirs);
    }
    if p.os == Os::Windows {
        let mut roots: Vec<PathBuf> = Vec::new();
        for program in ["bash", "git"] {
            if let Some(root) = find_executable(&[program], &path, p).and_then(|exe| posix_root(&exe)) {
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
        for key in ["ProgramFiles", "ProgramW6432"] {
            if let Some(pf) = vars.path(key) {
                let root = pf.join("Git");
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
        for root in roots {
            for sub in [&["usr", "bin"][..], &["bin"][..], &["mingw64", "bin"][..]] {
                let dir = sub.iter().fold(root.clone(), |d, s| d.join(s));
                if dir.is_dir() {
                    push(dir, &mut dirs);
                }
            }
        }
    }
    dirs
}

/// The installation root of a POSIX layout on Windows from one of its executables:
/// `<root>/bin/x.exe`, `<root>/cmd/x.exe`, `<root>/usr/bin/x.exe`, `<root>/mingw64/bin/x.exe`.
fn posix_root(exe: &Path) -> Option<PathBuf> {
    let parent = exe.parent()?;
    let up = parent.parent()?;
    let nested = up
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("usr") || n.eq_ignore_ascii_case("mingw64"));
    if nested {
        up.parent().map(Path::to_path_buf)
    } else {
        Some(up.to_path_buf())
    }
}

/// First existing `<dir>/<exe(name)>` over dirs x names (a regular file; on Unix with an
/// executable bit).
pub fn find_executable(names: &[&str], dirs: &[PathBuf], p: &Platform) -> Option<PathBuf> {
    for dir in dirs {
        for name in names {
            let candidate = dir.join(p.exe(name));
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Directories read by one [`ProgramDirs`] at most, and entries per directory.
const MAX_PROGRAM_DIRS: usize = 256;
const MAX_PROGRAM_DIR_ENTRIES: usize = 100_000;

/// Executable extensions of a Windows command lookup (`PATHEXT`, lower-case with the dot, in
/// order; the system default `.COM;.EXE;.BAT;.CMD` when unset). Empty on other systems.
pub(crate) fn path_extensions(vars: &EnvVars, p: &Platform) -> Vec<String> {
    if p.os != Os::Windows {
        return Vec::new();
    }
    let text = vars
        .get("PATHEXT")
        .and_then(OsStr::to_str)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(".COM;.EXE;.BAT;.CMD");
    let mut out: Vec<String> = Vec::new();
    for ext in text.split(';') {
        let ext = ext.trim().to_ascii_lowercase();
        if ext.len() > 1 && ext.starts_with('.') && !out.contains(&ext) {
            out.push(ext);
        }
    }
    out
}

/// Whether a command word can name a program found through PATH: a plain file name (letters,
/// digits and `-_.+@,%`; no directory part, expansion, quoting or assignment; not an option,
/// not `.` / `..`).
pub(crate) fn is_program_name(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 255
        && word != "."
        && word != ".."
        && !word.starts_with('-')
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+' | b'@' | b',' | b'%'))
}

/// Program lookup over PATH-like directories, the way a shell finds a command: each directory
/// is listed once (bounded), then lookups only confirm a hit with one file check. Nothing is
/// ever executed.
#[derive(Clone, Debug, Default)]
pub struct ProgramDirs {
    /// (directory, its entry names; ASCII case folded on Windows).
    dirs: Vec<(PathBuf, BTreeSet<String>)>,
    /// Windows `PATHEXT` extensions (empty elsewhere).
    extensions: Vec<String>,
    windows: bool,
}

impl ProgramDirs {
    /// List `dirs` (in order, each once; unreadable ones are skipped).
    pub fn read(dirs: &[PathBuf], vars: &EnvVars, p: &Platform) -> ProgramDirs {
        let windows = p.os == Os::Windows;
        let mut listed: Vec<(PathBuf, BTreeSet<String>)> = Vec::new();
        for dir in dirs.iter().take(MAX_PROGRAM_DIRS) {
            if listed.iter().any(|(d, _)| d == dir) {
                continue;
            }
            let Ok(rd) = fs::read_dir(dir) else { continue };
            let names: BTreeSet<String> = rd
                .filter_map(Result::ok)
                .take(MAX_PROGRAM_DIR_ENTRIES)
                .filter_map(|e| e.file_name().into_string().ok())
                .map(|n| if windows { n.to_ascii_lowercase() } else { n })
                .collect();
            listed.push((dir.clone(), names));
        }
        ProgramDirs {
            dirs: listed,
            extensions: path_extensions(vars, p),
            windows,
        }
    }

    /// The executable file `command` names in the first directory holding one: on Windows the
    /// name itself when it ends with a `PATHEXT` extension, else the name plus each extension
    /// in order; elsewhere the name, a file with an execute bit. `None` for words that are no
    /// plain program name ([`is_program_name`]).
    pub fn find(&self, command: &str) -> Option<PathBuf> {
        if !is_program_name(command) {
            return None;
        }
        let candidates: Vec<String> = if self.windows {
            let lower = command.to_ascii_lowercase();
            if self.extensions.iter().any(|e| lower.ends_with(e.as_str())) {
                vec![lower]
            } else {
                self.extensions.iter().map(|e| format!("{lower}{e}")).collect()
            }
        } else {
            vec![command.to_string()]
        };
        for (dir, names) in &self.dirs {
            for name in &candidates {
                if names.contains(name) {
                    let path = dir.join(name);
                    if is_executable_file(&path) {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    /// The listed directories in lookup order.
    pub fn dirs(&self) -> impl Iterator<Item = &Path> {
        self.dirs.iter().map(|(d, _)| d.as_path())
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Children of `dir` whose file name starts with `prefix`, newest version first (version
/// parsed from the rest of the name), e.g. ("C:/Program Files/R", "R-").
pub fn versioned_children(dir: &Path, prefix: &str) -> Vec<(Version, PathBuf)> {
    let mut out: Vec<(Version, PathBuf)> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let rest = name.strip_prefix(prefix)?;
                    Some((Version::parse(rest)?, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out
}

/// KEY=VALUE / KEY="VALUE" lines (release, pyvenv.cfg, go env, *.properties); '#' comments.
/// A missing or unreadable file is empty.
pub(crate) fn read_key_values(path: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(path)
        .map(|text| trace_core::formats::ini::key_values(&text))
        .unwrap_or_default()
}

/// Run a TOOLCHAIN binary (never project code) for its version: cwd = temp dir, env = PATH +
/// SystemRoot/TEMP only, 10 s timeout; stdout + stderr text. None when it cannot run, times
/// out or fails.
pub fn toolchain_output(exe: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .current_dir(std::env::temp_dir())
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in ["PATH", "SystemRoot", "SYSTEMROOT", "TEMP", "TMP", "TMPDIR"] {
        if let Some(v) = trace_core::env::var(key) {
            cmd.env(key, v);
        }
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = out_reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    let status = status?;
    if !status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&out).into_owned();
    text.push_str(&String::from_utf8_lossy(&err));
    Some(text)
}

/// A toolchain version: numeric parts, an optional pre-release tag (after '-'), and the
/// original text. Build metadata ('+...') and suffixes such as "p0" do not order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Version {
    pub parts: Vec<u64>,
    pub pre: Option<String>,
    pub text: String,
}

impl Version {
    /// "1.27.0", "go1.27.0", "3.4.10p0", "0.17.0-dev.1936+5a6", "21.0.12.1+1", "v0.23.0".
    pub fn parse(text: &str) -> Option<Version> {
        let trimmed = text.trim();
        let start = trimmed.find(|c: char| c.is_ascii_digit())?;
        let body = &trimmed[start..];
        let mut parts = Vec::new();
        let mut rest = body;
        loop {
            let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            if digits == 0 {
                break;
            }
            parts.push(rest[..digits].parse::<u64>().ok()?);
            rest = &rest[digits..];
            match rest.strip_prefix('.') {
                Some(next) if next.starts_with(|c: char| c.is_ascii_digit()) => rest = next,
                _ => break,
            }
        }
        if parts.is_empty() {
            return None;
        }
        let pre = rest
            .strip_prefix('-')
            .map(|p| p.split('+').next().unwrap_or_default().to_string());
        Some(Version {
            parts,
            pre: pre.filter(|p| !p.is_empty()),
            text: trimmed.to_string(),
        })
    }

    fn part(&self, i: usize) -> u64 {
        self.parts.get(i).copied().unwrap_or(0)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.parts.len().max(other.parts.len());
        for i in 0..n {
            match self.part(i).cmp(&other.part(i)) {
                Ordering::Equal => continue,
                o => return o,
            }
        }
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => a.cmp(b),
        }
        .then_with(|| self.text.cmp(&other.text))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A version requirement: `min` (inclusive) and/or `below` (exclusive).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionReq {
    pub min: Option<Version>,
    pub below: Option<Version>,
    pub text: String,
}

impl VersionReq {
    pub fn at_least(v: Version) -> VersionReq {
        VersionReq {
            text: format!(">= {}", v.text),
            min: Some(v),
            below: None,
        }
    }

    /// Numeric comparison (the original texts do not matter).
    pub fn matches(&self, v: &Version) -> bool {
        let numeric = |a: &Version, b: &Version| {
            let n = a.parts.len().max(b.parts.len());
            (0..n)
                .map(|i| a.part(i).cmp(&b.part(i)))
                .find(|o| *o != Ordering::Equal)
                .unwrap_or(Ordering::Equal)
                .then_with(|| match (&a.pre, &b.pre) {
                    (None, None) => Ordering::Equal,
                    (None, Some(_)) => Ordering::Greater,
                    (Some(_), None) => Ordering::Less,
                    (Some(x), Some(y)) => x.cmp(y),
                })
        };
        self.min.as_ref().is_none_or(|m| numeric(v, m) != Ordering::Less)
            && self.below.as_ref().is_none_or(|b| numeric(v, b) == Ordering::Less)
    }

    /// "Go 1.26 or newer", "Java 17 or newer, older than 26", "Go".
    pub fn describe(&self, tool: &str) -> String {
        match (&self.min, &self.below) {
            (Some(min), None) => format!("{tool} {} or newer", min.text),
            (None, Some(below)) => format!("{tool} older than {}", below.text),
            (Some(min), Some(below)) => {
                format!("{tool} {} or newer, older than {}", min.text, below.text)
            }
            (None, None) => tool.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/os.rs"]
mod tests;
