//! Stable workspaces for language servers (DESIGN §1.13, research platform-speed §1.3).
//!
//! Every backend has ONE workspace per repository, kept between runs so server and build
//! state survives (jdtls `-data`, Gradle / Bloop / `dist-newstyle` / `obj` caches, rust
//! `target`, CMake build dirs):
//!
//! ```text
//! <repo cache>/workspaces/<backend>/tree         the workspace root the server sees
//! <repo cache>/workspaces/<backend>/state        `{outside}`: server state that must not be
//!                                                inside the project root (never deleted by a sync)
//! <repo cache>/workspaces/<backend>/manifest.bin every file trace wrote into `tree`
//! <repo cache>/workspaces/<backend>/lock         OS file lock while a session uses the workspace
//! ```
//!
//! * `Snapshot` mode: the analysed source files of the partition plus the configuration
//!   files the entry names (`workspace.configs` globs), bytes verified against the index
//!   hash while copying.
//! * `Mirror` mode ([`crate::mirror`]): the whole repository tree (ignore rules,
//!   `include_ignored`, minus `exclude`, `.git`, sensitive files and symlinks), capped at
//!   4 GiB; analysed sources are written from the verified bytes the index hashed.
//!
//! Sync is incremental: files whose hash (snapshot) or size + mtime, then blake3 (mirror)
//! match the manifest are not touched; changed files are rewritten; files that left are
//! deleted (only files trace wrote: build outputs and auxiliary files stay). With watcher
//! hints ([`Snapshot::sync_hinted`]) a mirror only looks at the hinted paths. A different
//! stamp (trace version, backend, mode, mirror rules, tool fingerprint) wipes `tree` and
//! `state` first. The directory is never inside the inspected root and is never deleted
//! when the session ends (deleting the cache directory is the reset).
//!
//! Server names: a backend may give an analysed file another name in the tree
//! ([`WorkspaceOptions::server_name`]; Pyright sees an extensionless Python script `tool` as
//! `tool.py`, same bytes). The alias is used only when the repository has no file of that
//! name; [`Snapshot::path_of`] / URIs use it and [`Snapshot::relative`] maps it back, so
//! everything outside the workspace keeps the repository path. The manifest stays keyed by
//! the repository path and records the name written.
//!
//! After the backend answered, [`Snapshot::verify_originals`] / [`Snapshot::verify_paths`]
//! re-hash the originals of the analysed files: any change aborts the run with
//! `SemanticError::SourceChanged`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trace_core::inventory::strip_verbatim;
use trace_core::paths::ensure_outside;
use trace_core::source::read_verified;
use trace_core::{CoreError, Hash32, Language, SetupError};

use crate::languages::WorkspaceMode;
use crate::mirror::{max_mirror_bytes, MirrorEntry, MirrorRules};
use crate::SemanticError;

/// Envelope magic of workspace manifests.
const MANIFEST_MAGIC: [u8; 8] = *b"TRACEWSM";
/// Bump when the manifest layout or the workspace layout changes.
const MANIFEST_SCHEMA: u32 = 2;

/// A file to copy: relative path, expected hash.
#[derive(Clone, Debug)]
pub struct SnapshotFile<'a> {
    pub path: &'a str,
    pub hash: Hash32,
    pub bytes: &'a [u8],
}

/// How to open a backend's workspace.
#[derive(Clone, Debug)]
pub struct WorkspaceOptions<'a> {
    /// `<repo cache>/workspaces`.
    pub workspaces_dir: &'a Path,
    pub backend: &'a str,
    pub target_root: &'a Path,
    pub mode: WorkspaceMode,
    /// Tool fingerprint (and anything else whose change invalidates server state).
    pub stamp: &'a str,
    pub rules: MirrorRules,
    /// Language named by the size-cap error.
    pub language: Language,
    /// The workspace name of an analysed file when the server needs another one than its
    /// repository path (module docs); `None` (or an answer `None`) = its own path.
    pub server_name: Option<fn(&str) -> Option<String>>,
}

/// One file trace wrote into the tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestEntry {
    hash: Hash32,
    /// Size and mtime of the repository original (mirror quick check; 0 for analysed files).
    size: u64,
    mtime_ns: u64,
    /// Bytes written into the tree.
    written: u64,
    /// The name the file has in the tree when it is not its repository path (server names).
    alias: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    stamp: String,
    files: BTreeMap<String, ManifestEntry>,
}

/// A backend's open workspace.
#[derive(Debug)]
pub struct Snapshot {
    /// The workspace root the server sees (`.../<backend>/tree`).
    pub dir: PathBuf,
    base: PathBuf,
    state: PathBuf,
    repo_cache: PathBuf,
    target_root: PathBuf,
    mode: WorkspaceMode,
    rules: MirrorRules,
    language: Language,
    /// Analysed files (sources + configs) with the hash trace verified.
    files: BTreeMap<String, Hash32>,
    manifest: Manifest,
    /// [`WorkspaceOptions::server_name`].
    server_name: Option<fn(&str) -> Option<String>>,
    /// Tree name -> repository path of the files written under another name.
    originals: BTreeMap<String, String>,
    dirty: bool,
    /// Held while the workspace is in use (released on drop / process death).
    _lock: fs::File,
}

/// Files changed by a sync (sorted relative paths).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncDelta {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

impl SyncDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }

    /// Every path touched by the sync.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.added
            .iter()
            .chain(&self.changed)
            .chain(&self.removed)
            .map(String::as_str)
    }

    fn sort(&mut self) {
        self.added.sort();
        self.changed.sort();
        self.removed.sort();
    }
}

impl Snapshot {
    /// The stable snapshot-mode workspace of `backend` holding exactly `files` (used by
    /// backends without a tool stamp, e.g. the TypeScript worker).
    pub fn create(
        workspaces_dir: &Path,
        backend: &str,
        target_root: &Path,
        files: &[SnapshotFile<'_>],
    ) -> Result<Snapshot, SemanticError> {
        let opts = WorkspaceOptions {
            workspaces_dir,
            backend,
            target_root,
            mode: WorkspaceMode::Snapshot,
            stamp: "",
            rules: MirrorRules::default(),
            language: Language::Python,
            server_name: None,
        };
        Snapshot::open(&opts, files)
    }

    /// Open (lock, wipe on a stamp change) and sync the backend's workspace.
    pub fn open(opts: &WorkspaceOptions<'_>, files: &[SnapshotFile<'_>]) -> Result<Snapshot, SemanticError> {
        let mut snapshot = Snapshot::open_empty(opts)?;
        snapshot.sync(files)?;
        Ok(snapshot)
    }

    /// Lock and prepare the directories without syncing.
    fn open_empty(opts: &WorkspaceOptions<'_>) -> Result<Snapshot, SemanticError> {
        ensure_outside(opts.workspaces_dir, &[opts.target_root])?;
        let base = opts.workspaces_dir.join(sanitize(opts.backend));
        fs::create_dir_all(&base)?;
        let base = strip_verbatim(fs::canonicalize(&base)?);
        // Re-check after canonicalization (junctions/symlinks in the cache path).
        ensure_outside(&base, &[opts.target_root])?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(base.join("lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(SemanticError::Worker(format!(
                    "the {} workspace is in use by another trace process ({})",
                    opts.backend,
                    base.display()
                )))
            }
            Err(fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        let stamp = format!(
            "{}|{}|{:?}|{:?}|{:?}|{}",
            trace_core::TRACE_VERSION,
            opts.backend,
            opts.mode,
            opts.rules.include_ignored,
            opts.rules.exclude,
            opts.stamp
        );
        let tree = base.join("tree");
        let state = base.join("state");
        let manifest_path = base.join("manifest.bin");
        let loaded = if manifest_path.is_file() {
            trace_core::cache::load_blob::<Manifest>(&manifest_path, MANIFEST_MAGIC, MANIFEST_SCHEMA).ok()
        } else {
            None
        };
        let manifest = match loaded {
            Some(m) if m.stamp == stamp && tree.is_dir() => m,
            _ => {
                // Another version, tool or rule set: start from an empty tree and state.
                for dir in [&tree, &state] {
                    match fs::remove_dir_all(dir) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                let _ = fs::remove_file(&manifest_path);
                Manifest {
                    stamp,
                    files: BTreeMap::new(),
                }
            }
        };
        fs::create_dir_all(&tree)?;
        fs::create_dir_all(&state)?;
        let repo_cache = opts
            .workspaces_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| opts.workspaces_dir.to_path_buf());
        let mut snapshot = Snapshot {
            dir: tree,
            base,
            state,
            repo_cache,
            target_root: opts.target_root.to_path_buf(),
            mode: opts.mode,
            rules: opts.rules.clone(),
            language: opts.language,
            files: BTreeMap::new(),
            manifest,
            server_name: opts.server_name,
            originals: BTreeMap::new(),
            dirty: true,
            _lock: lock,
        };
        snapshot.drop_damaged();
        snapshot.originals = snapshot
            .manifest
            .files
            .iter()
            .filter_map(|(rel, e)| e.alias.clone().map(|alias| (alias, rel.clone())))
            .collect();
        Ok(snapshot)
    }

    /// Forget manifest entries whose tree file is gone or has another size (a server or
    /// build tool touched it): the next sync rewrites them.
    fn drop_damaged(&mut self) {
        let dir = self.dir.clone();
        self.manifest.files.retain(|rel, entry| {
            let mut path = dir.clone();
            path.extend(entry.alias.as_deref().unwrap_or(rel).split('/'));
            fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() == entry.written)
        });
    }

    /// Bring the workspace to exactly `files` (snapshot mode) or to the repository tree with
    /// `files` written from their verified bytes (mirror mode, full walk).
    pub fn sync(&mut self, files: &[SnapshotFile<'_>]) -> Result<SyncDelta, SemanticError> {
        self.sync_hinted(files, None)
    }

    /// [`Snapshot::sync`] with watcher hints: a mirror only stats the hinted repository
    /// paths instead of walking the whole tree (snapshot mode ignores hints).
    pub fn sync_hinted(
        &mut self,
        files: &[SnapshotFile<'_>],
        hints: Option<&[String]>,
    ) -> Result<SyncDelta, SemanticError> {
        let mut delta = SyncDelta::default();
        let mut wanted: HashSet<&str> = HashSet::with_capacity(files.len());
        let mut analysed: BTreeMap<String, Hash32> = BTreeMap::new();
        for file in files {
            if !wanted.insert(file.path) {
                continue;
            }
            analysed.insert(file.path.to_string(), file.hash);
            let alias = self.alias_for(file.path);
            match self.manifest.files.get(file.path) {
                Some(entry) if entry.hash == file.hash && entry.alias == alias => {}
                Some(_) => {
                    self.write_verified(file, alias)?;
                    delta.changed.push(file.path.to_string());
                }
                None => {
                    self.write_verified(file, alias)?;
                    delta.added.push(file.path.to_string());
                }
            }
        }
        match self.mode {
            WorkspaceMode::Snapshot => {
                let stale: Vec<String> = self
                    .manifest
                    .files
                    .keys()
                    .filter(|p| !wanted.contains(p.as_str()))
                    .cloned()
                    .collect();
                for path in stale {
                    self.remove(&path)?;
                    delta.removed.push(path);
                }
            }
            WorkspaceMode::Mirror => self.sync_mirror(&wanted, hints, &mut delta)?,
        }
        self.files = analysed;
        delta.sort();
        delta.added.dedup();
        delta.changed.dedup();
        delta.removed.dedup();
        self.save()?;
        Ok(delta)
    }

    /// Mirror mode: copy repository files (other than the analysed ones), delete those
    /// that left, enforce the size cap.
    fn sync_mirror(
        &mut self,
        wanted: &HashSet<&str>,
        hints: Option<&[String]>,
        delta: &mut SyncDelta,
    ) -> Result<(), SemanticError> {
        let (present, gone): (Vec<MirrorEntry>, Vec<String>) = match hints {
            Some(paths) => crate::mirror::stat_paths(&self.target_root, &self.rules, paths),
            None => {
                let present = crate::mirror::walk(&self.target_root, &self.rules);
                let seen: BTreeSet<&str> = present.iter().map(|e| e.rel.as_str()).collect();
                let gone = self
                    .manifest
                    .files
                    .keys()
                    .filter(|p| !seen.contains(p.as_str()) && !wanted.contains(p.as_str()))
                    .cloned()
                    .collect();
                (present, gone)
            }
        };
        for entry in &present {
            if wanted.contains(entry.rel.as_str()) {
                continue;
            }
            let known = self.manifest.files.get(&entry.rel).cloned();
            if known
                .as_ref()
                .is_some_and(|k| k.size == entry.size && k.mtime_ns == entry.mtime_ns)
            {
                continue;
            }
            let Ok(bytes) = fs::read(&entry.abs) else {
                // Vanished between the walk and the read: treated as gone.
                if known.is_some() {
                    self.remove(&entry.rel)?;
                    delta.removed.push(entry.rel.clone());
                }
                continue;
            };
            let hash = Hash32::of(&bytes);
            let record = ManifestEntry {
                hash,
                size: entry.size,
                mtime_ns: entry.mtime_ns,
                written: bytes.len() as u64,
                alias: None,
            };
            match known {
                Some(k) if k.hash == hash && k.alias.is_none() => {
                    // Touched, same content: remember the new stamp only.
                    self.manifest.files.insert(entry.rel.clone(), record);
                    self.dirty = true;
                }
                Some(_) => {
                    self.write_bytes(&entry.rel, &bytes, record)?;
                    delta.changed.push(entry.rel.clone());
                }
                None => {
                    self.write_bytes(&entry.rel, &bytes, record)?;
                    delta.added.push(entry.rel.clone());
                }
            }
        }
        for rel in gone {
            if wanted.contains(rel.as_str()) || !self.manifest.files.contains_key(&rel) {
                continue;
            }
            self.remove(&rel)?;
            delta.removed.push(rel);
        }
        let total: u64 = self.manifest.files.values().map(|e| e.written).sum();
        if total > max_mirror_bytes() {
            return Err(SemanticError::Setup(SetupError::Unsupported {
                language: self.language,
                first: format!(
                    "This repository is too large for trace to analyze ({}).",
                    crate::mirror::size_text(total)
                ),
                second: None,
            }));
        }
        Ok(())
    }

    /// Relative paths of the analysed files currently in the workspace (sorted).
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// Whether `rel` is an analysed source/config file of the workspace.
    pub fn contains(&self, rel: &str) -> bool {
        self.files.contains_key(rel)
    }

    /// The workspace mode.
    pub fn mode(&self) -> WorkspaceMode {
        self.mode
    }

    /// `<repo cache>` (the `{repo_cache}` placeholder).
    pub fn repo_cache(&self) -> &Path {
        &self.repo_cache
    }

    /// The tree name of analysed file `rel` when the server needs another one: the backend's
    /// [`WorkspaceOptions::server_name`], valid, and naming no file of the repository (so it
    /// never hides or replaces a repository file).
    fn alias_for(&self, rel: &str) -> Option<String> {
        let name = (self.server_name?)(rel)?;
        if name == rel || self.dest_path(&name).is_err() {
            return None;
        }
        let mut original = self.target_root.clone();
        original.extend(name.split('/'));
        if fs::symlink_metadata(&original).is_ok() {
            return None;
        }
        Some(name)
    }

    fn write_verified(
        &mut self,
        file: &SnapshotFile<'_>,
        alias: Option<String>,
    ) -> Result<(), SemanticError> {
        if Hash32::of(file.bytes) != file.hash {
            return Err(SemanticError::SourceChanged(file.path.to_string()));
        }
        let record = ManifestEntry {
            hash: file.hash,
            size: 0,
            mtime_ns: 0,
            written: file.bytes.len() as u64,
            alias,
        };
        self.write_bytes(file.path, file.bytes, record)
    }

    fn write_bytes(&mut self, rel: &str, bytes: &[u8], record: ManifestEntry) -> Result<(), SemanticError> {
        let dest = self.dest_path(record.alias.as_deref().unwrap_or(rel))?;
        // Written under another name before: that copy goes.
        let old_alias = self.manifest.files.get(rel).map(|e| e.alias.clone());
        if let Some(old) = old_alias.filter(|old| *old != record.alias) {
            self.remove_tree_file(old.as_deref().unwrap_or(rel))?;
            if let Some(old) = old {
                self.originals.remove(&old);
            }
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, bytes)?;
        if let Some(alias) = &record.alias {
            self.originals.insert(alias.clone(), rel.to_string());
        }
        self.manifest.files.insert(rel.to_string(), record);
        self.dirty = true;
        Ok(())
    }

    fn remove(&mut self, rel: &str) -> Result<(), SemanticError> {
        let alias = self.manifest.files.get(rel).and_then(|e| e.alias.clone());
        self.remove_tree_file(alias.as_deref().unwrap_or(rel))?;
        if let Some(alias) = alias {
            self.originals.remove(&alias);
        }
        self.manifest.files.remove(rel);
        self.dirty = true;
        Ok(())
    }

    /// Delete the tree file `name` (already gone is fine).
    fn remove_tree_file(&self, name: &str) -> Result<(), SemanticError> {
        let dest = self.dest_path(name)?;
        match fs::remove_file(&dest) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Persist the manifest when it changed.
    fn save(&mut self) -> Result<(), SemanticError> {
        if !self.dirty {
            return Ok(());
        }
        trace_core::cache::save_blob(
            &self.base.join("manifest.bin"),
            MANIFEST_MAGIC,
            MANIFEST_SCHEMA,
            &self.manifest,
        )?;
        self.dirty = false;
        Ok(())
    }

    /// Absolute path of a relative file inside the workspace (the server name of a file
    /// written under another name, module docs).
    pub fn path_of(&self, rel: &str) -> PathBuf {
        let name = self
            .manifest
            .files
            .get(rel)
            .and_then(|e| e.alias.as_deref())
            .unwrap_or(rel);
        let mut path = self.dir.clone();
        path.extend(name.split('/').filter(|p| !p.is_empty()));
        path
    }

    /// Map an absolute path inside the workspace back to its relative (repository) path.
    pub fn relative(&self, abs: &Path) -> Option<String> {
        let rel = crate::mapping::relative_to(&self.dir, abs)?;
        Some(self.originals.get(&rel).cloned().unwrap_or(rel))
    }

    /// Re-hash originals of every analysed file under the target root; error if any changed.
    pub fn verify_originals(&self) -> Result<(), SemanticError> {
        self.verify_paths(self.files.keys().map(String::as_str))
    }

    /// Re-hash the originals of the given analysed files (unknown paths are ignored).
    pub fn verify_paths<'p>(&self, paths: impl IntoIterator<Item = &'p str>) -> Result<(), SemanticError> {
        for rel in paths {
            let Some(hash) = self.files.get(rel) else {
                continue;
            };
            match read_verified(&self.target_root, rel, hash) {
                Ok(_) => {}
                Err(CoreError::SourceChanged(path)) => return Err(SemanticError::SourceChanged(path)),
                Err(CoreError::Io { .. }) | Err(CoreError::Symlink(_)) => {
                    return Err(SemanticError::SourceChanged(rel.to_string()))
                }
                Err(other) => return Err(other.into()),
            }
        }
        Ok(())
    }

    /// Write an auxiliary file (controlled configuration, worker input) into the workspace.
    /// Auxiliary files are not in the manifest: syncs never delete them.
    pub fn write_aux(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, SemanticError> {
        let path = self.dest_path(name)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::read(&path).is_ok_and(|old| old == bytes) {
            return Ok(path);
        }
        fs::write(&path, bytes)?;
        Ok(path)
    }

    /// The stable per-backend state directory `{outside}` (server data that must NOT live
    /// inside the project root: Eclipse/jdtls rejects a `-data` workspace that overlaps the
    /// project location). Never deleted by a sync.
    pub fn outside_dir(&self) -> PathBuf {
        self.state.clone()
    }

    /// Validated destination for a `/`-separated relative path.
    fn dest_path(&self, rel: &str) -> Result<PathBuf, SemanticError> {
        let invalid = || SemanticError::Core(CoreError::InvalidRelativePath(rel.to_string()));
        if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') || rel.contains(':') {
            return Err(invalid());
        }
        let mut path = self.dir.clone();
        for part in rel.split('/') {
            if part.is_empty() || part == "." || part == ".." {
                return Err(invalid());
            }
            path.push(part);
        }
        Ok(path)
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = self.save();
    }
}

/// Directory-safe backend name (`lsp:gopls` -> `lsp-gopls`).
pub fn sanitize(backend: &str) -> String {
    backend
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/unit/snapshot.rs"]
mod tests;
