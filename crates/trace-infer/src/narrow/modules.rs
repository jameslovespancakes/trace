//! Module map of an index: which files an import target of a file names, by the
//! module path rules of the file's language (`LanguageRules::modules`; child of
//! [`crate::narrow`]).

use super::*;
use trace_core::relpath;

/// Resolution of import targets to repository files (module paths, packages, relative
/// specifiers). Syntax only: paths are matched, nothing is loaded or executed.
pub struct ModuleMap {
    /// Stem path (path without extension; `pkg/__init__`, `dir/index`, `dir/mod` also as
    /// `pkg`, `dir`) -> files.
    stems: HashMap<String, Vec<FileId>>,
    /// Last stem segment -> stem paths.
    by_last: HashMap<String, Vec<String>>,
    /// Directory -> files directly inside.
    dirs: HashMap<String, Vec<FileId>>,
    /// Last directory segment -> directories.
    dirs_by_last: HashMap<String, Vec<String>>,
    /// Language of every file (index = `FileId`).
    languages: Vec<Language>,
    /// Rust workspace crates: crate directory name (`-` spelled `_`) -> source roots
    /// (`<crate>/src`).
    rust_crates: HashMap<String, Vec<String>>,
}

fn stem_of(path: &str) -> &str {
    let name_start = path.rfind('/').map_or(0, |i| i + 1);
    match path[name_start..].rfind('.') {
        Some(dot) if dot > 0 => &path[..name_start + dot],
        _ => path,
    }
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl ModuleMap {
    pub fn new(index: &Index) -> ModuleMap {
        let mut map = ModuleMap {
            stems: HashMap::new(),
            by_last: HashMap::new(),
            dirs: HashMap::new(),
            dirs_by_last: HashMap::new(),
            languages: index.files.iter().map(|f| f.language).collect(),
            rust_crates: HashMap::new(),
        };
        for (fi, f) in index.files.iter().enumerate() {
            let id = FileId(fi as u32);
            let stem = stem_of(&f.path).to_string();
            let dir = relpath::parent(&f.path).to_string();
            let mut stems = vec![stem.clone()];
            let package_file = matches!(last_segment(&stem), "__init__" | "index" | "mod" | "lib" | "main");
            if package_file {
                stems.push(dir.clone());
            }
            // Declaration files: `m.d.ts` answers the specifier `./m`.
            let declaration = rules(f.language).declaration_file_suffix;
            if !declaration.is_empty() {
                if let Some(plain) = stem.strip_suffix(declaration) {
                    stems.push(plain.to_string());
                }
            }
            if rules(f.language).modules == ModulePaths::CratePaths
                && (stem == "src/lib" || stem.ends_with("/src/lib"))
            {
                let crate_dir = relpath::parent(&dir);
                if !crate_dir.is_empty() {
                    map.rust_crates
                        .entry(last_segment(crate_dir).replace('-', "_"))
                        .or_default()
                        .push(dir.clone());
                }
            }
            for s in stems {
                if s.is_empty() {
                    continue;
                }
                let entry = map.stems.entry(s.clone()).or_default();
                if entry.is_empty() {
                    map.by_last
                        .entry(last_segment(&s).to_string())
                        .or_default()
                        .push(s.clone());
                }
                entry.push(id);
            }
            let entry = map.dirs.entry(dir.clone()).or_default();
            if entry.is_empty() && !dir.is_empty() {
                map.dirs_by_last
                    .entry(last_segment(&dir).to_string())
                    .or_default()
                    .push(dir.clone());
            }
            entry.push(id);
        }
        map
    }

    /// Files whose stem (or package directory) ends with `segments` on segment boundaries.
    fn suffix(&self, segments: &[&str], packages: bool) -> Vec<FileId> {
        let Some(last) = segments.last() else {
            return Vec::new();
        };
        let wanted = segments.join("/");
        let ends = |p: &str| p == wanted || p.ends_with(&format!("/{wanted}"));
        let mut out: Vec<FileId> = Vec::new();
        for stem in self.by_last.get(*last).map(Vec::as_slice).unwrap_or(&[]) {
            if ends(stem) {
                out.extend(self.stems.get(stem).map(Vec::as_slice).unwrap_or(&[]));
            }
        }
        if packages {
            for dir in self.dirs_by_last.get(*last).map(Vec::as_slice).unwrap_or(&[]) {
                if ends(dir) {
                    out.extend(self.dirs.get(dir).map(Vec::as_slice).unwrap_or(&[]));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub(super) fn exact(&self, stem: &str) -> Vec<FileId> {
        let mut out: Vec<FileId> = self.stems.get(stem).cloned().unwrap_or_default();
        out.extend(self.dirs.get(stem).map(Vec::as_slice).unwrap_or(&[]));
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Files a relative script specifier (`./x`, `../x.js`, `.`, `..`) of the file at
    /// `from_path` names by Node.js file resolution: the path itself or with a script
    /// extension, else the directory's `index` file (a file wins over a directory index).
    pub(crate) fn script_files(&self, index: &Index, from_path: &str, specifier: &str) -> Vec<FileId> {
        let Some(joined) = relpath::lexical(&format!("{}/{specifier}", relpath::parent(from_path))) else {
            return Vec::new();
        };
        let stem = stem_of_specifier(&joined);
        let index_stem = if stem.is_empty() {
            "index".to_string()
        } else {
            format!("{stem}/index")
        };
        let named = |wanted: &str| -> Vec<FileId> {
            let mut out: Vec<FileId> = self
                .stems
                .get(wanted)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .copied()
                .filter(|&f| stem_of(index.file_path(f)) == wanted)
                .collect();
            out.sort_unstable();
            out.dedup();
            out
        };
        let direct = if stem.is_empty() { Vec::new() } else { named(stem) };
        if direct.is_empty() {
            named(&index_stem)
        } else {
            direct
        }
    }

    /// Files an import target of a file in `language` at `from_path` refers to, and whether
    /// the whole target (not just its module part) matched (it names a module, not a member).
    /// `member_import`: the last segment may be a member of the module.
    pub fn resolve(
        &self,
        from_path: &str,
        language: Language,
        target: &str,
        member_import: bool,
    ) -> Option<(Vec<FileId>, bool)> {
        let target = target.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`');
        if target.is_empty() {
            return None;
        }
        let from_dir = relpath::parent(from_path);
        match rules(language).modules {
            // `source` / `.` paths: relative to the sourcing script first; otherwise (a
            // variable directory prefix was dropped by trace-syntax) every file whose path ends
            // with the literal tail. Absolute paths are outside the repository.
            ModulePaths::SourcedFiles => {
                let t = target.trim_start_matches("./");
                if t.starts_with('/') || t.is_empty() {
                    return None;
                }
                let stem = stem_of(t);
                let joined = if from_dir.is_empty() {
                    relpath::lexical(stem)?
                } else {
                    relpath::lexical(&format!("{from_dir}/{stem}"))?
                };
                let files = self.exact(&joined);
                if !files.is_empty() {
                    return Some((files, true));
                }
                let mut out: Vec<FileId> = Vec::new();
                for s in self.by_last.get(last_segment(stem)).map(Vec::as_slice).unwrap_or(&[]) {
                    if s == stem || s.ends_with(&format!("/{stem}")) {
                        out.extend(self.stems.get(s).map(Vec::as_slice).unwrap_or(&[]));
                    }
                }
                out.sort_unstable();
                out.dedup();
                (!out.is_empty()).then_some((out, true))
            }
            ModulePaths::RelativeSpecifiers => {
                // `<specifier>.<export>` for members; `<specifier>` for namespaces.
                let spec = if member_import {
                    target.rsplit_once('.').map_or(target, |(s, _)| s)
                } else {
                    target
                };
                if !(spec.starts_with("./") || spec.starts_with("../")) {
                    return None;
                }
                let joined = relpath::lexical(&format!("{from_dir}/{spec}"))?;
                let files = self.exact(stem_of_specifier(&joined));
                (!files.is_empty()).then_some((files, !member_import))
            }
            ModulePaths::DottedModules => {
                let dots = target.chars().take_while(|&c| c == '.').count();
                let rest = &target[dots..];
                let segments: Vec<&str> = rest.split('.').filter(|s| !s.is_empty()).collect();
                if dots > 0 {
                    let mut base = from_dir.to_string();
                    for _ in 1..dots {
                        base = relpath::parent(&base).to_string();
                    }
                    let full = if segments.is_empty() {
                        base.clone()
                    } else if base.is_empty() {
                        segments.join("/")
                    } else {
                        format!("{base}/{}", segments.join("/"))
                    };
                    let files = self.exact(&full);
                    if !files.is_empty() {
                        return Some((files, true));
                    }
                    if member_import && !segments.is_empty() {
                        let module = if segments.len() == 1 {
                            base
                        } else {
                            format!(
                                "{}{}{}",
                                base,
                                if base.is_empty() { "" } else { "/" },
                                segments[..segments.len() - 1].join("/")
                            )
                        };
                        let files = self.exact(&module);
                        return (!files.is_empty()).then_some((files, false));
                    }
                    return None;
                }
                self.resolve_segments(&segments, member_import, false)
            }
            ModulePaths::ImportPaths => {
                let segments: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
                for k in (1..=segments.len()).rev() {
                    let dirs = self.suffix_dirs(&segments[segments.len() - k..]);
                    if !dirs.is_empty() {
                        return Some((dirs, true));
                    }
                }
                None
            }
            ModulePaths::CratePaths => self
                .resolve_crate_path(from_path, language, target, member_import)
                .or_else(|| self.resolve_default(language, target, member_import)),
            ModulePaths::Global
            | ModulePaths::PackageDirectories
            | ModulePaths::NamespaceDirectories
            | ModulePaths::ModuleFiles => self.resolve_default(language, target, member_import),
        }
    }

    /// Dotted / path targets of the remaining languages: longest stem suffix (module files
    /// and package directories), then, where the language maps packages / namespaces to
    /// directories (Java, Scala, Haskell, PHP: [`ModulePaths::packages_are_directories`]),
    /// the package directory.
    fn resolve_default(
        &self,
        language: Language,
        target: &str,
        member_import: bool,
    ) -> Option<(Vec<FileId>, bool)> {
        let segments: Vec<&str> = target
            .split(['.', '/', '\\', ':'])
            .filter(|s| !s.is_empty() && *s != "crate" && *s != "self" && *s != "super")
            .collect();
        if let Some(found) = self.resolve_segments(&segments, member_import, true) {
            return Some(found);
        }
        if !rules(language).modules.packages_are_directories() {
            return None;
        }
        let namespace = namespace_segments(language, target);
        let files = self.namespace_files(&namespace, language);
        if !files.is_empty() {
            return Some((files, true));
        }
        if member_import && namespace.len() > 1 {
            let files = self.namespace_files(&namespace[..namespace.len() - 1], language);
            if !files.is_empty() {
                return Some((files, false));
            }
        }
        None
    }

    /// Whether file `f` shares a name namespace with `language`.
    fn interop(&self, language: Language, f: FileId) -> bool {
        self.languages
            .get(f.idx())
            .is_some_and(|&l| name_interop(language, l))
    }

    /// Module files of exactly this stem path (`a/b` -> `a/b.<ext>`, `a/b/__init__.py`,
    /// `a/b/index.ts`, `a/b/mod.rs`), never the other files of a directory.
    fn module_files(&self, path: &str, language: Language) -> Vec<FileId> {
        let mut out: Vec<FileId> = self
            .stems
            .get(path)
            .map(|v| v.iter().copied().filter(|&f| self.interop(language, f)).collect())
            .unwrap_or_default();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Module files whose stem path ends with `segments` (segment boundaries).
    fn stem_suffix(&self, segments: &[&str], language: Language) -> Vec<FileId> {
        let Some(last) = segments.last() else {
            return Vec::new();
        };
        let wanted = segments.join("/");
        let mut out: Vec<FileId> = Vec::new();
        for stem in self.by_last.get(*last).map(Vec::as_slice).unwrap_or(&[]) {
            if *stem == wanted || stem.ends_with(&format!("/{wanted}")) {
                out.extend(
                    self.stems
                        .get(stem)
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                        .iter()
                        .copied()
                        .filter(|&f| self.interop(language, f)),
                );
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Python top-level module `name`: a module or package at the repository root, or a
    /// package (`__init__.py`) one directory below it (`src/<name>`); never deeper and never
    /// a plain module file in a subdirectory (a same-named file elsewhere, e.g.
    /// `utils/json.py`, is not what `import json` loads).
    fn top_level_module(&self, name: &str, language: Language) -> Vec<FileId> {
        let mut out: Vec<FileId> = Vec::new();
        for stem in self.by_last.get(name).map(Vec::as_slice).unwrap_or(&[]) {
            let shallow = match stem.strip_suffix(name) {
                Some("") => true,
                Some(prefix) => {
                    prefix.ends_with('/')
                        && !prefix[..prefix.len() - 1].contains('/')
                        && self.stems.contains_key(&format!("{stem}/__init__"))
                }
                None => false,
            };
            if shallow {
                out.extend(
                    self.stems
                        .get(stem)
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                        .iter()
                        .copied()
                        .filter(|&f| self.interop(language, f)),
                );
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Files directly inside the directories whose path ends with `segments` (a package
    /// mapped to directories), in the name namespace of `language`.
    pub fn package_files(&self, segments: &[&str], language: Language) -> Vec<FileId> {
        let Some(last) = segments.last() else {
            return Vec::new();
        };
        let wanted = segments.join("/");
        let mut out: Vec<FileId> = Vec::new();
        for dir in self.dirs_by_last.get(*last).map(Vec::as_slice).unwrap_or(&[]) {
            if *dir == wanted || dir.ends_with(&format!("/{wanted}")) {
                out.extend(
                    self.dirs
                        .get(dir)
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                        .iter()
                        .copied()
                        .filter(|&f| self.interop(language, f)),
                );
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Package / namespace directory files: JVM and Haskell packages end with the whole
    /// path; PHP namespaces (PSR-4) drop a vendor prefix mapped to a source root, so the
    /// longest matching suffix of at least one segment counts.
    fn namespace_files(&self, segments: &[&str], language: Language) -> Vec<FileId> {
        if rules(language).modules != ModulePaths::NamespaceDirectories {
            return self.package_files(segments, language);
        }
        for start in 0..segments.len() {
            let files = self.package_files(&segments[start..], language);
            if !files.is_empty() {
                return files;
            }
        }
        Vec::new()
    }

    fn has_crate_stem(&self, dir: &str, name: &str) -> bool {
        let key = if dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}/{name}")
        };
        self.stems.get(&key).is_some_and(|files| {
            files.iter().any(|&f| {
                self.languages
                    .get(f.idx())
                    .is_some_and(|&l| rules(l).modules == ModulePaths::CratePaths)
            })
        })
    }

    /// Source root of the Rust crate containing `from_path`: the nearest directory holding
    /// `lib.rs` / `main.rs`. `None` for `bin/` targets (their own crate roots) and files
    /// outside a crate source tree.
    fn rust_crate_root(&self, from_path: &str) -> Option<String> {
        let mut dir = relpath::parent(from_path).to_string();
        for _ in 0..64 {
            if self.has_crate_stem(&dir, "lib") || self.has_crate_stem(&dir, "main") {
                let rel = if dir.is_empty() {
                    from_path
                } else {
                    from_path.strip_prefix(&format!("{dir}/")).unwrap_or(from_path)
                };
                if rel.starts_with("bin/") {
                    return None;
                }
                return Some(dir);
            }
            if dir.is_empty() {
                return None;
            }
            dir = relpath::parent(&dir).to_string();
        }
        None
    }

    /// Module path of a Rust file (`src/a/b.rs` -> `src/a/b`, `src/a/mod.rs` -> `src/a`,
    /// the crate root file -> the source root).
    fn rust_module_path(&self, from_path: &str) -> Option<String> {
        let stem = stem_of(from_path);
        let dir = relpath::parent(from_path);
        match last_segment(stem) {
            "mod" => Some(dir.to_string()),
            "lib" | "main" if self.rust_crate_root(from_path).as_deref() == Some(dir) => {
                Some(dir.to_string())
            }
            _ => Some(stem.to_string()),
        }
    }

    /// Module path a Rust `use` path starts from and how many leading segments it consumed:
    /// `crate::` (crate root), `self::` (this module), `super::` (parents), with `workspace`
    /// a workspace crate named like its directory (its source root), else the current
    /// module (2018 paths to child modules).
    fn rust_base(&self, from_path: &str, segments: &[&str], workspace: bool) -> Option<(String, usize)> {
        let first = *segments.first()?;
        match first {
            "crate" => Some((self.rust_crate_root(from_path)?, 1)),
            "self" => Some((self.rust_module_path(from_path)?, 1)),
            "super" => {
                let root = self.rust_crate_root(from_path)?;
                let mut module = self.rust_module_path(from_path)?;
                let mut used = 0;
                while segments.get(used) == Some(&"super") {
                    if module == root {
                        return None;
                    }
                    module = relpath::parent(&module).to_string();
                    used += 1;
                }
                Some((module, used))
            }
            name => {
                if let Some(roots) = self.rust_crates.get(&name.replace('-', "_")).filter(|_| workspace) {
                    return match roots.as_slice() {
                        [only] => Some((only.clone(), 1)),
                        _ => None,
                    };
                }
                Some((self.rust_module_path(from_path)?, 0))
            }
        }
    }

    /// Rust `use` paths (`crate::`, `self::`, `super::`, child modules): the module files of
    /// the whole path, else (member imports) of the path without its last segment. Other
    /// crates keep the suffix rule of [`ModuleMap::resolve_default`] (a crate's name is
    /// its manifest's, not its directory's).
    fn resolve_crate_path(
        &self,
        from_path: &str,
        language: Language,
        target: &str,
        member_import: bool,
    ) -> Option<(Vec<FileId>, bool)> {
        let segments: Vec<&str> = target.split("::").map(str::trim).filter(|s| !s.is_empty()).collect();
        let (base, used) = self.rust_base(from_path, &segments, false)?;
        let rest = &segments[used..];
        let files = self.module_files(&join_path(&base, &rest.join("/")), language);
        if !files.is_empty() {
            return Some((files, true));
        }
        if member_import && !rest.is_empty() {
            let module = join_path(&base, &rest[..rest.len() - 1].join("/"));
            let files = self.module_files(&module, language);
            if !files.is_empty() {
                return Some((files, false));
            }
        }
        None
    }

    /// Import-path rule (SPEC §7.12): every split of an import / re-export `target` of a
    /// file of `language` at `from_path` into (module files, member qualified name) that
    /// the language's module path rules allow:
    /// * Python: relative `..p.x` from the importing package, absolute `a.b.x` by module
    ///   suffix (a single-segment module only at the root or one directory below);
    /// * JavaScript / TypeScript: relative specifiers (`<specifier>.<export>`; extensions,
    ///   `index` files, `.d.ts`), never `default`;
    /// * Rust: `crate::` / `self::` / `super::` / workspace crate / child-module paths, the
    ///   member being every suffix of the path (`crate::a::T::f` -> `a.rs` + `T.f`);
    /// * Java, Scala: package directories (`okio.TestUtil.randomBytes` -> files of
    ///   `.../okio/` + `TestUtil.randomBytes`);
    /// * PHP: namespace directories (PSR-4); Haskell: module files (`Data.Map.lookup`).
    ///
    /// Other languages (C / C++ includes, Go packages, C# namespaces) import modules, not
    /// members: nothing.
    pub(crate) fn member_paths(
        &self,
        from_path: &str,
        language: Language,
        target: &str,
    ) -> Vec<(Vec<FileId>, String)> {
        fn push(out: &mut Vec<(Vec<FileId>, String)>, files: Vec<FileId>, member: String) {
            if !files.is_empty() && !member.is_empty() {
                out.push((files, member));
            }
        }
        let target = target.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`');
        let mut out: Vec<(Vec<FileId>, String)> = Vec::new();
        if target.is_empty() {
            return out;
        }
        let from_dir = relpath::parent(from_path);
        match rules(language).modules {
            ModulePaths::DottedModules => {
                let dots = target.chars().take_while(|&c| c == '.').count();
                let segments: Vec<&str> = target[dots..].split('.').filter(|s| !s.is_empty()).collect();
                if let Some((member, module)) = segments.split_last() {
                    if dots > 0 {
                        let mut base = from_dir.to_string();
                        for _ in 1..dots {
                            base = relpath::parent(&base).to_string();
                        }
                        let path = join_path(&base, &module.join("/"));
                        push(&mut out, self.module_files(&path, language), member.to_string());
                    } else if module.len() >= 2 {
                        push(&mut out, self.stem_suffix(module, language), member.to_string());
                    } else if let Some(top) = module.first() {
                        push(&mut out, self.top_level_module(top, language), member.to_string());
                    }
                }
            }
            ModulePaths::RelativeSpecifiers => {
                if let Some((spec, name)) = target.rsplit_once('.') {
                    let relative = spec.starts_with("./") || spec.starts_with("../");
                    if relative && !name.is_empty() && name != "default" && !name.contains('/') {
                        if let Some(joined) = relpath::lexical(&format!("{from_dir}/{spec}")) {
                            let files = self.module_files(stem_of_specifier(&joined), language);
                            push(&mut out, files, name.to_string());
                        }
                    }
                }
            }
            ModulePaths::CratePaths => {
                let segments: Vec<&str> =
                    target.split("::").map(str::trim).filter(|s| !s.is_empty()).collect();
                if let Some((base, used)) = self.rust_base(from_path, &segments, true) {
                    let rest = &segments[used..];
                    for k in (0..rest.len()).rev() {
                        let path = join_path(&base, &rest[..k].join("/"));
                        push(&mut out, self.module_files(&path, language), rest[k..].join("."));
                    }
                }
            }
            ModulePaths::PackageDirectories => {
                let segments: Vec<&str> = target.split('.').filter(|s| !s.is_empty()).collect();
                for k in 1..segments.len() {
                    push(&mut out, self.package_files(&segments[..k], language), segments[k..].join("."));
                }
            }
            ModulePaths::NamespaceDirectories => {
                let segments: Vec<&str> = namespace_segments(language, target);
                for k in 1..segments.len() {
                    push(&mut out, self.namespace_files(&segments[..k], language), segments[k..].join("."));
                }
            }
            ModulePaths::ModuleFiles => {
                let segments: Vec<&str> = target.split('.').filter(|s| !s.is_empty()).collect();
                if let Some((member, module)) = segments.split_last() {
                    if !module.is_empty() {
                        push(&mut out, self.stem_suffix(module, language), member.to_string());
                    }
                }
            }
            ModulePaths::Global | ModulePaths::SourcedFiles | ModulePaths::ImportPaths => {}
        }
        out
    }

    fn suffix_dirs(&self, segments: &[&str]) -> Vec<FileId> {
        let Some(last) = segments.last() else {
            return Vec::new();
        };
        let wanted = segments.join("/");
        let mut out = Vec::new();
        for dir in self.dirs_by_last.get(*last).map(Vec::as_slice).unwrap_or(&[]) {
            if *dir == wanted || dir.ends_with(&format!("/{wanted}")) {
                out.extend(self.dirs.get(dir).map(Vec::as_slice).unwrap_or(&[]));
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Absolute dotted target: longest suffix match of at least two segments (one for
    /// single-segment targets); member imports may drop their last segment.
    fn resolve_segments(
        &self,
        segments: &[&str],
        member_import: bool,
        packages: bool,
    ) -> Option<(Vec<FileId>, bool)> {
        if segments.is_empty() {
            return None;
        }
        let min = segments.len().min(2);
        let files = self.suffix(segments, packages);
        if !files.is_empty() {
            return Some((files, true));
        }
        if member_import && segments.len() > min {
            let module = &segments[..segments.len() - 1];
            let files = self.suffix(module, packages);
            if !files.is_empty() {
                return Some((files, false));
            }
        }
        None
    }
}

/// `dir/file.js` -> `dir/file` (explicit extensions of the languages with relative
/// specifiers).
fn stem_of_specifier(path: &str) -> &str {
    let name_start = path.rfind('/').map_or(0, |i| i + 1);
    match path[name_start..].rfind('.') {
        Some(dot)
            if dot > 0
                && Language::ALL.iter().any(|&l| {
                    rules(l).modules == ModulePaths::RelativeSpecifiers
                        && trace_core::languages::info(l)
                            .extensions
                            .contains(&&path[name_start + dot + 1..])
                }) =>
        {
            &path[..name_start + dot]
        }
        _ => path,
    }
}

/// `base/rest` without empty segments at the joint.
fn join_path(base: &str, rest: &str) -> String {
    match (base.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (_, true) => base.to_string(),
        _ => format!("{base}/{rest}"),
    }
}

/// Segments of a package / namespace path (`a.b.C`; PHP `\A\B\C`).
fn namespace_segments(language: Language, target: &str) -> Vec<&str> {
    let separator = rules(language).modules.namespace_separator();
    target
        .trim_start_matches(separator)
        .split(separator)
        .filter(|s| !s.is_empty())
        .collect()
}
