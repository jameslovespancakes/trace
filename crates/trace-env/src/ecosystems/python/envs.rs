//! The search for the project's Python environment (see the module docs of `python` for the
//! order): virtual environments, PEP 582, Poetry, Pipenv, conda and pyenv environments.

use super::*;

/// The environment of the project (see the module docs for the order). `Err(path)` when the
/// `--env` override is not a Python environment.
pub(crate) fn find_environment(cx: &DetectContext<'_>) -> Result<Option<PythonEnv>, PathBuf> {
    if let Some(path) = cx.env_override {
        return match env_at(path, Origin::Override) {
            Some(env) => Ok(Some(env)),
            None => Err(path.to_path_buf()),
        };
    }
    let root = cx.root;
    let ok = |env: Option<PythonEnv>| env.filter(|e| cx.allowed(&e.root));
    // 2. uv's explicit project environment.
    if let Some(value) = cx.vars.get("UV_PROJECT_ENVIRONMENT").and_then(|v| v.to_str()) {
        if !value.trim().is_empty() {
            let p = PathBuf::from(value.trim());
            let p = if p.is_absolute() { p } else { root.join(p) };
            if let Some(env) = ok(env_at(&p, Origin::Project)) {
                return Ok(Some(env));
            }
        }
    }
    // 3. In-project virtual environments.
    if let Some(env) = ok(find_venv(root)) {
        return Ok(Some(env));
    }
    // 4. PEP 582.
    if let Some(env) = ok(pypackages(root)) {
        return Ok(Some(env));
    }
    // 5. Poetry.
    if let Some(env) = ok(poetry_env(root, cx.vars, cx.platform)) {
        return Ok(Some(env));
    }
    // 6. Pipenv.
    if let Some(env) = ok(pipenv_env(root, cx.vars, cx.platform)) {
        return Ok(Some(env));
    }
    // 7. Conda environment.yml.
    if let Some(env) = ok(conda_env(root, cx.vars, cx.platform)) {
        return Ok(Some(env));
    }
    // 8. pyenv-virtualenv.
    if let Some(env) = ok(pyenv_env(root, cx.vars, cx.platform)) {
        return Ok(Some(env));
    }
    // 9. The activated shell.
    for key in ["VIRTUAL_ENV", "CONDA_PREFIX"] {
        if let Some(p) = cx.vars.path(key) {
            if let Some(env) = ok(env_at(&p, Origin::Path)) {
                return Ok(Some(env));
            }
        }
    }
    Ok(None)
}

/// The shallowest virtual environment at the root or one level below, by `VENV_NAMES`
/// order. Only directories are considered (a `.env` *file* is never opened).
pub(crate) fn find_venv(root: &Path) -> Option<PythonEnv> {
    let at = |dir: &Path| {
        VENV_NAMES
            .iter()
            .map(|n| dir.join(n))
            .filter(|p| p.is_dir())
            .find_map(|p| python_env(&p, Origin::Project))
    };
    at(root).or_else(|| {
        subdirs(root)
            .into_iter()
            .filter(|(n, _)| {
                !n.starts_with('.') && !SKIP_DIRS.contains(&n.as_str()) && !VENV_NAMES.contains(&n.as_str())
            })
            .find_map(|(_, p)| at(&p))
    })
}

/// Any accepted environment directory at `dir`: a virtual or conda environment, a
/// `site-packages` directory, or a PEP 582 `__pypackages__/<X.Y>/lib`.
pub(crate) fn env_at(dir: &Path, origin: Origin) -> Option<PythonEnv> {
    if let Some(env) = python_env(dir, origin) {
        return Some(env);
    }
    let name = dir.file_name()?.to_string_lossy().to_string();
    if !dir.is_dir() {
        return None;
    }
    if name.eq_ignore_ascii_case("site-packages") {
        // `lib/pythonX.Y/site-packages` names its version.
        let version = dir
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .and_then(|n| n.strip_prefix("python").map(str::to_string))
            .and_then(|v| major_minor(&v));
        return Some(PythonEnv {
            root: dir.to_path_buf(),
            site_packages: vec![dir.to_path_buf()],
            version,
            origin,
            kind: EnvKind::SitePackages,
            system_site_packages: Vec::new(),
            stdlib: None,
        });
    }
    if name == "__pypackages__" {
        return pypackages_in(dir, origin);
    }
    None
}

/// A virtual environment (`pyvenv.cfg`) or a conda environment (`conda-meta/`) with its
/// `site-packages`.
pub(crate) fn python_env(dir: &Path, origin: Origin) -> Option<PythonEnv> {
    let cfg_path = dir.join("pyvenv.cfg");
    let (kind, cfg) = if cfg_path.is_file() {
        (EnvKind::Venv, os::read_key_values(&cfg_path))
    } else if dir.join("conda-meta").is_dir() {
        (EnvKind::Conda, BTreeMap::new())
    } else {
        return None;
    };
    let mut version = cfg
        .get("version_info")
        .and_then(|v| major_minor(v))
        .or_else(|| cfg.get("version").and_then(|v| major_minor(v)));
    if kind == EnvKind::Conda {
        version = version.or_else(|| conda_python_version(dir));
    }
    let mut site_packages = Vec::new();
    let windows = dir.join("Lib").join("site-packages");
    if windows.is_dir() {
        site_packages.push(windows);
    }
    for (name, path) in subdirs(&dir.join("lib")) {
        let sp = path.join("site-packages");
        if name.starts_with("python") && sp.is_dir() {
            if version.is_none() {
                version = name.strip_prefix("python").and_then(major_minor);
            }
            site_packages.push(sp);
        }
    }
    site_packages.sort();
    site_packages.dedup();
    if site_packages.is_empty() {
        return None;
    }
    let mut system_site_packages = Vec::new();
    let mut stdlib = None;
    let home = cfg.get("home").map(PathBuf::from).filter(|h| h.is_absolute());
    if kind == EnvKind::Venv {
        if let Some(home) = &home {
            let (sp, lib) = base_layout(home, version.as_deref());
            if cfg
                .get("include-system-site-packages")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
            {
                system_site_packages.extend(sp);
            }
            stdlib = lib;
        }
    } else {
        stdlib = stdlib_dir(dir, version.as_deref());
    }
    Some(PythonEnv {
        root: dir.to_path_buf(),
        site_packages,
        version,
        origin,
        kind,
        system_site_packages,
        stdlib,
    })
}

/// The base interpreter's `site-packages` and standard library below `home` (the directory
/// of the interpreter): Windows `home/Lib`, Unix `home/../lib/pythonX.Y`.
pub(super) fn base_layout(home: &Path, version: Option<&str>) -> (Option<PathBuf>, Option<PathBuf>) {
    let mut libs = vec![home.join("Lib")];
    if let Some(prefix) = home.parent() {
        match version {
            Some(v) => libs.push(prefix.join("lib").join(format!("python{v}"))),
            None => {
                for (name, path) in subdirs(&prefix.join("lib")) {
                    if name.starts_with("python") {
                        libs.push(path);
                    }
                }
            }
        }
    }
    let lib = libs.into_iter().find(|l| l.join("os.py").is_file());
    let sp = lib.as_ref().map(|l| l.join("site-packages")).filter(|p| p.is_dir());
    (sp, lib)
}

/// A conda environment's standard library (`Lib/` or `lib/pythonX.Y/`).
pub(super) fn stdlib_dir(env: &Path, version: Option<&str>) -> Option<PathBuf> {
    let mut candidates = vec![env.join("Lib")];
    if let Some(v) = version {
        candidates.push(env.join("lib").join(format!("python{v}")));
    }
    candidates.into_iter().find(|l| l.join("os.py").is_file())
}

/// `conda-meta/python-3.12.4-h1234_0.json` -> "3.12".
pub(super) fn conda_python_version(env: &Path) -> Option<String> {
    let mut best: Option<Version> = None;
    for (name, _) in entries(&env.join("conda-meta")) {
        let Some(rest) = name.strip_prefix("python-") else {
            continue;
        };
        if !name.ends_with(".json") || !rest.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let text = rest.split('-').next().unwrap_or_default();
        if let Some(v) = Version::parse(text) {
            if best.as_ref().is_none_or(|b| v > *b) {
                best = Some(v);
            }
        }
    }
    best.and_then(|v| major_minor(&v.text))
}

/// PEP 582: the newest `__pypackages__/<X.Y>/lib` of the project.
pub(super) fn pypackages(root: &Path) -> Option<PythonEnv> {
    pypackages_in(&root.join("__pypackages__"), Origin::Project)
}

pub(super) fn pypackages_in(dir: &Path, origin: Origin) -> Option<PythonEnv> {
    let mut best: Option<(Version, PathBuf)> = None;
    for (name, path) in subdirs(dir) {
        let lib = path.join("lib");
        let Some(v) = Version::parse(&name) else {
            continue;
        };
        if lib.is_dir() && best.as_ref().is_none_or(|(b, _)| v > *b) {
            best = Some((v, lib));
        }
    }
    let (v, lib) = best?;
    Some(PythonEnv {
        root: lib.clone(),
        site_packages: vec![lib],
        version: major_minor(&v.text),
        origin,
        kind: EnvKind::PyPackages,
        system_site_packages: Vec::new(),
        stdlib: None,
    })
}

/// Poetry's shared virtualenv: `<virtualenvs.path>/<sanitized name>-<h8>-py<X.Y>`.
pub(super) fn poetry_env(root: &Path, vars: &EnvVars, platform: &Platform) -> Option<PythonEnv> {
    let pyproject = toml_file(&root.join("pyproject.toml"))?;
    let poetry = pyproject.get("tool").and_then(|t| t.get("poetry"));
    if poetry.is_none() && !root.join("poetry.lock").is_file() {
        return None;
    }
    let name = poetry
        .and_then(|p| p.get("name"))
        .or_else(|| pyproject.get("project").and_then(|p| p.get("name")))
        .and_then(Value::as_str)?;
    let base = poetry_virtualenvs_dir(vars, platform)?;
    let env_name = poetry_env_name(name, &poetry_normcase(root, platform));
    // envs.toml names the active minor version per environment name.
    let active = toml_file(&base.join("envs.toml")).and_then(|v| {
        v.get(&env_name)
            .and_then(|e| e.get("minor"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let prefix = format!("{env_name}-py");
    let mut candidates: Vec<(Version, PathBuf)> = subdirs(&base)
        .into_iter()
        .filter_map(|(n, p)| Some((Version::parse(n.strip_prefix(&prefix)?)?, p)))
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    if let Some(minor) = active {
        if let Some((_, p)) = candidates
            .iter()
            .find(|(v, _)| major_minor(&v.text).as_deref() == Some(minor.as_str()))
        {
            return python_env(p, Origin::UserCache);
        }
    }
    candidates
        .into_iter()
        .find_map(|(_, p)| python_env(&p, Origin::UserCache))
}

/// `POETRY_VIRTUALENVS_PATH` | `config.toml` `virtualenvs.path` | `<cache dir>/virtualenvs`.
pub(super) fn poetry_virtualenvs_dir(vars: &EnvVars, platform: &Platform) -> Option<PathBuf> {
    if let Some(p) = vars.path("POETRY_VIRTUALENVS_PATH") {
        return Some(p);
    }
    let cache = vars.path("POETRY_CACHE_DIR").or_else(|| {
        Some(match platform.os {
            Os::Windows => vars.path("LOCALAPPDATA")?.join("pypoetry").join("Cache"),
            _ => os::cache_dir(vars, platform)?.join("pypoetry"),
        })
    });
    let config_dir = vars.path("POETRY_CONFIG_DIR").or_else(|| {
        Some(match platform.os {
            Os::Windows => vars.path("APPDATA")?.join("pypoetry"),
            _ => os::config_dir(vars, platform)?.join("pypoetry"),
        })
    });
    if let Some(config) = config_dir.and_then(|d| toml_file(&d.join("config.toml"))) {
        let path = config
            .get("virtualenvs")
            .and_then(|v| v.get("path"))
            .or_else(|| config.get("virtualenvs.path"))
            .and_then(Value::as_str);
        if let Some(path) = path {
            let expanded = match &cache {
                Some(c) => path.replace("{cache-dir}", &c.display().to_string()),
                None => path.to_string(),
            };
            let p = PathBuf::from(expanded);
            if p.is_absolute() {
                return Some(p);
            }
        }
    }
    cache.map(|c| c.join("virtualenvs"))
}

/// Poetry's environment name: the lower-cased project name with `` $`!*@"\\\r\n\t`` replaced
/// by `_`, cut to 42 characters, `-`, then the first 8 characters of the URL-safe base64 of
/// sha256(normalised project path).
pub(crate) fn poetry_env_name(name: &str, normalized_path: &str) -> String {
    let sanitized: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if matches!(c, ' ' | '$' | '`' | '!' | '*' | '@' | '"' | '\\' | '\r' | '\n' | '\t') {
                '_'
            } else {
                c
            }
        })
        .take(42)
        .collect();
    let digest = Sha256::digest(normalized_path.as_bytes());
    let encoded = base64_urlsafe(&digest);
    format!("{sanitized}-{}", &encoded[..8])
}

/// Python's `os.path.normcase(os.path.realpath(root))` without running Python: the
/// canonical path without the Windows `\\?\` prefix, lower-cased with `\` on Windows.
pub(super) fn poetry_normcase(root: &Path, platform: &Platform) -> String {
    let real = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let text = real.display().to_string();
    if platform.os == Os::Windows {
        let text = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
        };
        text.replace('/', "\\").to_lowercase()
    } else {
        text
    }
}

/// RFC 4648 URL-safe base64 with padding (Python's `base64.urlsafe_b64encode`).
pub(super) fn base64_urlsafe(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, v) in idx.iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[*v as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Pipenv: `<WORKON_HOME | ~/.virtualenvs>/<dir name>-*` whose `.project` file names the
/// project directory.
pub(super) fn pipenv_env(root: &Path, vars: &EnvVars, platform: &Platform) -> Option<PythonEnv> {
    if !root.join("Pipfile").is_file() {
        return None;
    }
    let base = vars
        .path("WORKON_HOME")
        .or_else(|| os::home_dir(vars, platform).map(|h| h.join(".virtualenvs")))?;
    let dir_name = root.file_name()?.to_string_lossy().to_string();
    let want = comparable_path(root, platform);
    subdirs(&base)
        .into_iter()
        .filter(|(n, _)| n.starts_with(&format!("{dir_name}-")))
        .filter(|(_, p)| {
            fs::read_to_string(p.join(".project"))
                .map(|text| comparable_path(Path::new(text.trim()), platform) == want)
                .unwrap_or(false)
        })
        .find_map(|(_, p)| python_env(&p, Origin::UserCache))
}

pub(super) fn comparable_path(path: &Path, platform: &Platform) -> String {
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = real.display().to_string().replace('\\', "/");
    let text = text
        .strip_prefix("//?/")
        .unwrap_or(&text)
        .trim_end_matches('/')
        .to_string();
    if platform.os == Os::Windows {
        text.to_lowercase()
    } else {
        text
    }
}

/// Conda: `environment.yml` `prefix:` or `name:` -> `<base>/envs/<name>`.
pub(super) fn conda_env(root: &Path, vars: &EnvVars, platform: &Platform) -> Option<PythonEnv> {
    let text = ["environment.yml", "environment.yaml"]
        .iter()
        .find_map(|n| fs::read_to_string(root.join(n)).ok())?;
    let doc = yaml::parse(&text)?;
    if let Some(prefix) = doc.get("prefix").and_then(Value::as_str) {
        let p = PathBuf::from(prefix);
        if let Some(env) = python_env(&p, Origin::UserCache) {
            return Some(env);
        }
    }
    let name = doc.get("name").and_then(Value::as_str)?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    // environments.txt lists every environment path (all OS).
    if let Some(home) = os::home_dir(vars, platform) {
        if let Ok(list) = fs::read_to_string(home.join(".conda").join("environments.txt")) {
            for line in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let p = PathBuf::from(line);
                if p.file_name().is_some_and(|n| n.to_string_lossy() == name) {
                    if let Some(env) = python_env(&p, Origin::UserCache) {
                        return Some(env);
                    }
                }
            }
        }
    }
    conda_bases(vars, platform)
        .into_iter()
        .find_map(|base| python_env(&base.join("envs").join(&name), Origin::UserCache))
}

/// Conda installations: `CONDA_PREFIX` / `CONDA_EXE` / `MAMBA_ROOT_PREFIX`, `~/.conda`, and
/// the standard install directories of the OS.
pub(super) fn conda_bases(vars: &EnvVars, platform: &Platform) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(prefix) = vars.path("CONDA_PREFIX") {
        // `<base>/envs/<name>` -> `<base>`; otherwise the prefix is the base.
        match prefix.parent() {
            Some(parent) if parent.file_name().is_some_and(|n| n == "envs") => {
                if let Some(base) = parent.parent() {
                    out.push(base.to_path_buf());
                }
            }
            _ => out.push(prefix.clone()),
        }
    }
    if let Some(exe) = vars.path("CONDA_EXE") {
        // `<base>/Scripts/conda.exe` | `<base>/bin/conda`.
        if let Some(base) = exe.parent().and_then(Path::parent) {
            out.push(base.to_path_buf());
        }
    }
    if let Some(p) = vars.path("MAMBA_ROOT_PREFIX") {
        out.push(p);
    }
    let home = os::home_dir(vars, platform);
    if let Some(h) = &home {
        out.push(h.join(".conda"));
    }
    let names = ["miniconda3", "anaconda3", "miniforge3", "mambaforge"];
    match platform.os {
        Os::Windows => {
            for n in names {
                if let Some(h) = &home {
                    out.push(h.join(n));
                }
                if let Some(l) = vars.path("LOCALAPPDATA") {
                    out.push(l.join(n));
                }
                if let Some(pd) = vars.path("ProgramData") {
                    out.push(pd.join(n));
                }
            }
        }
        Os::Linux | Os::MacOs => {
            for n in names {
                if let Some(h) = &home {
                    out.push(h.join(n));
                }
            }
            out.push(PathBuf::from("/opt/conda"));
            if platform.os == Os::MacOs {
                out.push(PathBuf::from("/opt/homebrew/Caskroom/miniforge/base"));
            }
        }
    }
    out.dedup();
    out
}

/// pyenv-virtualenv: `.python-version` naming an environment (not a version).
pub(super) fn pyenv_env(root: &Path, vars: &EnvVars, platform: &Platform) -> Option<PythonEnv> {
    let text = fs::read_to_string(root.join(".python-version")).ok()?;
    let name = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    if name.starts_with(|c: char| c.is_ascii_digit()) || name.contains(['/', '\\']) {
        return None;
    }
    let mut dirs = Vec::new();
    if let Some(p) = vars.path("PYENV_ROOT") {
        dirs.push(p.join("versions").join(name));
    }
    if let Some(h) = os::home_dir(vars, platform) {
        dirs.push(h.join(".pyenv").join("versions").join(name));
        dirs.push(h.join(".pyenv").join("pyenv-win").join("versions").join(name));
    }
    dirs.into_iter().find_map(|d| python_env(&d, Origin::Pin))
}

pub(super) fn is_store_stub_dir(dir: &Path) -> bool {
    dir.to_string_lossy()
        .replace('\\', "/")
        .to_lowercase()
        .ends_with("/microsoft/windowsapps")
}

pub(super) fn version_from_dir_name(dir: &Path) -> Option<Version> {
    // `Python312` -> 3.12, `cpython-3.14-windows-x86_64-none` -> 3.14.
    let name = dir.file_name()?.to_string_lossy().to_string();
    if let Some(rest) = name.strip_prefix("Python") {
        if rest.len() >= 2 && rest.chars().all(|c| c.is_ascii_digit()) {
            return Version::parse(&format!("{}.{}", &rest[..1], &rest[1..]));
        }
    }
    let rest = name.strip_prefix("cpython-")?;
    Version::parse(rest.split('-').next()?)
}

/// "3.12.4" / "3.12" -> "3.12".
pub(super) fn major_minor(text: &str) -> Option<String> {
    let parts: Vec<&str> = text.trim().split('.').take(2).collect();
    (parts.len() == 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())))
    .then(|| parts.join("."))
}

/// `.python-version` naming a version ("3.12.1" -> "3.12").
pub(super) fn pinned_version(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join(".python-version")).ok()?;
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    let first = first.strip_prefix("cpython-").unwrap_or(first);
    major_minor(first.split('-').next().unwrap_or(first))
}

pub(super) fn toml_file(path: &Path) -> Option<Value> {
    toml_value(&fs::read_to_string(path).ok()?)
}
