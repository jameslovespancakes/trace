//! Archive extraction (owner install): zip (also `.sit`, `.vsix`, `.nupkg`), tar.gz, tar.xz,
//! a single gzip-compressed file and a raw binary, the format taken from the magic bytes
//! (never from the file name). `strip` / `strip_prefix` / `subdir` select the tree to keep.
//!
//! Safety (DESIGN §1.9 item 5): absolute paths, drive prefixes, `..`, Windows device names and
//! alternate data streams are rejected (the whole archive is refused); symlinks are created
//! only when their target stays inside the destination (on Unix; skipped on Windows); hard
//! links, devices and fifos are refused; the unpacked size is capped
//! (`semantic.max_unpacked_mb`); Unix modes come from the archive headers and declared
//! executables get their exec bits (`mark_executables`). Nothing from an archive is run.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use trace_env::os::{Os, Platform};

/// Largest total size an install may unpack in bytes (setting `semantic.max_unpacked_mb`).
fn max_unpacked_bytes() -> u64 {
    trace_core::config::current().semantic.max_unpacked_mb << 20
}

/// Archive formats recognised from magic bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Zip,
    Gzip,
    Xz,
    Raw,
}

/// The format of `bytes` from its magic bytes.
pub fn detect(bytes: &[u8]) -> Format {
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        Format::Zip
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        Format::Gzip
    } else if bytes.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Format::Xz
    } else {
        Format::Raw
    }
}

/// Whether a 512-byte block starts a tar archive (`ustar` magic at offset 257).
fn is_tar(block: &[u8]) -> bool {
    block.len() >= 262 && &block[257..262] == b"ustar"
}

/// What to keep from an archive and where single files go.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtractOptions<'a> {
    /// Leading path components dropped from every entry (entries with fewer are skipped).
    pub strip: u32,
    /// Only entries below this prefix are kept, the prefix removed (macOS JDK
    /// `jdk-21.0.12.1+1/Contents/Home/`).
    pub strip_prefix: Option<&'a str>,
    /// Only this tree is kept, the prefix removed (nupkg `tools/net10.0/win-x64`).
    pub subdir: Option<&'a str>,
    /// Destination (relative to `dest`) of a raw binary or a single gzip-compressed file.
    pub single_file: Option<&'a str>,
}

/// Why an archive was refused or could not be unpacked.
#[derive(Debug)]
pub enum ArchiveError {
    /// A path or entry type that could escape the destination.
    Unsafe(String),
    /// More than `semantic.max_unpacked_mb`.
    TooLarge,
    /// The bytes are not a readable archive of the detected format.
    Corrupt(String),
    Io(io::Error),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::Unsafe(m) => write!(f, "refused archive entry: {m}"),
            ArchiveError::TooLarge => {
                write!(f, "archive unpacks to more than {} bytes", max_unpacked_bytes())
            }
            ArchiveError::Corrupt(m) => write!(f, "unreadable archive: {m}"),
            ArchiveError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for ArchiveError {
    fn from(e: io::Error) -> Self {
        ArchiveError::Io(e)
    }
}

/// Remaining unpacked bytes of one install.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub remaining: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            remaining: max_unpacked_bytes(),
        }
    }
}

impl Budget {
    fn take(&mut self, n: u64) -> Result<(), ArchiveError> {
        if n > self.remaining {
            return Err(ArchiveError::TooLarge);
        }
        self.remaining -= n;
        Ok(())
    }
}

/// Unpack `archive` into the existing directory `dest`.
pub fn extract(
    archive: &Path,
    dest: &Path,
    opts: &ExtractOptions<'_>,
    budget: &mut Budget,
) -> Result<Format, ArchiveError> {
    let mut head = [0u8; 8];
    let n = {
        let mut f = File::open(archive)?;
        read_up_to(&mut f, &mut head)?
    };
    let format = detect(&head[..n]);
    match format {
        Format::Zip => extract_zip(File::open(archive)?, dest, opts, budget)?,
        Format::Gzip => {
            let mut decoder = flate2::read::MultiGzDecoder::new(BufReader::new(File::open(archive)?));
            let mut block = vec![0u8; 512];
            let got =
                read_up_to(&mut decoder, &mut block).map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
            block.truncate(got);
            let reader = io::Cursor::new(block.clone()).chain(decoder);
            if is_tar(&block) {
                extract_tar(reader, dest, opts, budget)?;
            } else {
                write_single(reader, dest, opts, budget)?;
            }
        }
        Format::Xz => {
            // lzma-rs decodes into a writer: unpack into a temporary file next to the
            // extracted tree (same volume), capped by the budget, then read it as tar.
            let tmp = dest.join(format!(".trace-xz-{}", uuid::Uuid::new_v4().simple()));
            let result = (|| {
                {
                    let mut input = BufReader::new(File::open(archive)?);
                    let mut out = LimitedWriter {
                        inner: io::BufWriter::new(File::create(&tmp)?),
                        budget: budget.remaining,
                    };
                    lzma_rs::xz_decompress(&mut input, &mut out).map_err(|e| {
                        if out.budget == 0 {
                            ArchiveError::TooLarge
                        } else {
                            ArchiveError::Corrupt(format!("{e:?}"))
                        }
                    })?;
                    out.inner.flush()?;
                }
                let mut block = vec![0u8; 512];
                let got = read_up_to(&mut File::open(&tmp)?, &mut block)?;
                block.truncate(got);
                if is_tar(&block) {
                    extract_tar(BufReader::new(File::open(&tmp)?), dest, opts, budget)
                } else {
                    write_single(BufReader::new(File::open(&tmp)?), dest, opts, budget)
                }
            })();
            let _ = fs::remove_file(&tmp);
            result?;
        }
        Format::Raw => write_single(BufReader::new(File::open(archive)?), dest, opts, budget)?,
    }
    Ok(format)
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// A writer that fails once `budget` bytes were written.
struct LimitedWriter<W: Write> {
    inner: W,
    budget: u64,
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() as u64 > self.budget {
            self.budget = 0;
            return Err(io::Error::other("unpacked size limit"));
        }
        let n = self.inner.write(buf)?;
        self.budget -= n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A raw binary or single decompressed file -> `dest/<single_file>`.
fn write_single(
    reader: impl Read,
    dest: &Path,
    opts: &ExtractOptions<'_>,
    budget: &mut Budget,
) -> Result<(), ArchiveError> {
    let rel = opts.single_file.ok_or_else(|| {
        ArchiveError::Corrupt("a single-file artifact needs a declared executable name".into())
    })?;
    let rel = sanitize(rel)?;
    if rel.is_empty() {
        return Err(ArchiveError::Unsafe("empty single-file name".into()));
    }
    let path = join(dest, &rel);
    write_file(reader, &path, budget)?;
    Ok(())
}

/// Windows device names (refused as any path component, on every OS).
const DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
    "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "conin$", "conout$",
];

/// Split an archive path into safe components (`.` and empty parts dropped). Absolute paths,
/// drive prefixes, `..`, device names, `:` (alternate data streams) and control characters
/// are refused.
pub(crate) fn sanitize(raw: &str) -> Result<Vec<String>, ArchiveError> {
    let unsafe_path = || ArchiveError::Unsafe(raw.to_string());
    if raw.starts_with('/') || raw.starts_with('\\') {
        return Err(unsafe_path());
    }
    let mut out = Vec::new();
    for part in raw.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') || part.chars().any(char::is_control) {
            return Err(unsafe_path());
        }
        let stem = part
            .split('.')
            .next()
            .unwrap_or(part)
            .trim_end_matches([' ', '.'])
            .to_ascii_lowercase();
        if DEVICE_NAMES.contains(&stem.as_str()) {
            return Err(unsafe_path());
        }
        out.push(part.to_string());
    }
    Ok(out)
}

/// The kept relative path of an entry (None = not selected), after `strip_prefix`, `subdir`
/// and `strip`.
fn select(raw: &str, opts: &ExtractOptions<'_>) -> Result<Option<Vec<String>>, ArchiveError> {
    let mut parts = sanitize(raw)?;
    for prefix in [opts.strip_prefix, opts.subdir].into_iter().flatten() {
        let want = sanitize(prefix)?;
        if parts.len() < want.len() || parts[..want.len()] != want[..] {
            return Ok(None);
        }
        parts.drain(..want.len());
    }
    let strip = opts.strip as usize;
    if parts.len() <= strip {
        return Ok(None);
    }
    parts.drain(..strip);
    Ok(Some(parts))
}

fn join(dest: &Path, parts: &[String]) -> PathBuf {
    let mut p = dest.to_path_buf();
    for part in parts {
        p.push(part);
    }
    p
}

/// The target of a symlink at `link` (relative parts) resolves inside the destination.
fn link_stays_inside(link: &[String], target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') || target.starts_with('\\') || target.contains(':') {
        return false;
    }
    let mut stack: Vec<&str> = link[..link.len().saturating_sub(1)]
        .iter()
        .map(String::as_str)
        .collect();
    for part in target.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                if stack.pop().is_none() {
                    return false;
                }
            }
            other => stack.push(other),
        }
    }
    true
}

fn write_file(mut reader: impl Read, path: &Path, budget: &mut Budget) -> Result<u64, ArchiveError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(ArchiveError::Unsafe(format!("{} would write through a symlink", path.display())));
    }
    let mut file = io::BufWriter::new(File::create(path)?);
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ArchiveError::Corrupt(e.to_string())),
        };
        if n == 0 {
            break;
        }
        budget.take(n as u64)?;
        file.write_all(&buf[..n])?;
        total += n as u64;
    }
    file.flush()?;
    Ok(total)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Always owner-readable/writable; keep the archive's exec bits.
    fs::set_permissions(path, fs::Permissions::from_mode((mode & 0o777) | 0o600))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &str, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _ = fs::remove_file(path);
    std::os::unix::fs::symlink(target, path)
}

#[cfg(not(unix))]
fn make_symlink(_target: &str, _path: &Path) -> io::Result<()> {
    // Creating symlinks needs a privilege on Windows; the runtimes and servers trace installs
    // do not need theirs (Node's bin/npm, bin/npx), so they are skipped.
    Ok(())
}

fn extract_tar(
    reader: impl Read,
    dest: &Path,
    opts: &ExtractOptions<'_>,
    budget: &mut Budget,
) -> Result<(), ArchiveError> {
    use tar::EntryType;
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
        let kind = entry.header().entry_type();
        if matches!(
            kind,
            EntryType::XGlobalHeader | EntryType::XHeader | EntryType::GNULongName | EntryType::GNULongLink
        ) {
            continue;
        }
        let raw = entry
            .path()
            .map_err(|e| ArchiveError::Corrupt(e.to_string()))?
            .to_string_lossy()
            .into_owned();
        // Refuse unsafe names even outside the selected tree.
        let Some(parts) = select(&raw, opts)? else {
            continue;
        };
        let path = join(dest, &parts);
        match kind {
            EntryType::Directory => fs::create_dir_all(&path)?,
            EntryType::Regular | EntryType::Continuous => {
                let mode = entry.header().mode().unwrap_or(0o644);
                write_file(&mut entry, &path, budget)?;
                set_mode(&path, mode)?;
            }
            EntryType::Symlink => {
                let target = entry
                    .link_name()
                    .map_err(|e| ArchiveError::Corrupt(e.to_string()))?
                    .map(|t| t.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !link_stays_inside(&parts, &target) {
                    return Err(ArchiveError::Unsafe(format!("{raw} -> {target}")));
                }
                make_symlink(&target, &path)?;
            }
            EntryType::Link => {
                return Err(ArchiveError::Unsafe(format!("hard link {raw}")));
            }
            other => {
                return Err(ArchiveError::Unsafe(format!("{raw} ({other:?})")));
            }
        }
    }
    Ok(())
}

fn extract_zip(
    file: File,
    dest: &Path,
    opts: &ExtractOptions<'_>,
    budget: &mut Budget,
) -> Result<(), ArchiveError> {
    let mut archive =
        zip::ZipArchive::new(BufReader::new(file)).map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
        let raw = entry.name().to_string();
        let Some(parts) = select(&raw, opts)? else {
            continue;
        };
        let path = join(dest, &parts);
        if entry.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        if entry.size() > budget.remaining {
            return Err(ArchiveError::TooLarge);
        }
        if entry.is_symlink() {
            let mut target = String::new();
            entry
                .read_to_string(&mut target)
                .map_err(|e| ArchiveError::Corrupt(e.to_string()))?;
            if !link_stays_inside(&parts, &target) {
                return Err(ArchiveError::Unsafe(format!("{raw} -> {target}")));
            }
            make_symlink(&target, &path)?;
            continue;
        }
        let mode = entry.unix_mode();
        write_file(&mut entry, &path, budget)?;
        if let Some(mode) = mode {
            set_mode(&path, mode)?;
        }
    }
    Ok(())
}

/// Give a file exec bits (Unix; no-op elsewhere).
pub(crate) fn set_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)?.permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o755))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// The file a declared executable path names under `dir` (`.exe` tried first on Windows
/// when the name has no extension).
pub(crate) fn executable_path(dir: &Path, rel: &str, platform: &Platform) -> Option<PathBuf> {
    let parts = sanitize(rel).ok()?;
    if parts.is_empty() {
        return None;
    }
    let path = join(dir, &parts);
    let mut candidates = Vec::new();
    if platform.os == Os::Windows && path.extension().is_none() {
        candidates.push(path.with_extension("exe"));
    }
    candidates.push(path);
    candidates.into_iter().find(|p| p.is_file())
}

/// Where a single-file artifact is written for the declared executable `rel` (`.exe` added on
/// Windows when the name has no extension).
pub(crate) fn single_file_name(rel: &str, platform: &Platform) -> String {
    let has_ext = Path::new(rel).extension().is_some();
    if platform.os == Os::Windows && !has_ext {
        format!("{rel}.exe")
    } else {
        rel.to_string()
    }
}

/// Set exec bits on every declared executable that exists under `dir`; returns
/// (declared path, file) for the ones found.
pub(crate) fn mark_executables(
    dir: &Path,
    executables: &[String],
    platform: &Platform,
) -> io::Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    for rel in executables {
        if let Some(path) = executable_path(dir, rel, platform) {
            set_executable(&path)?;
            found.push((rel.clone(), path));
        }
    }
    Ok(found)
}

/// Remove a directory tree, clearing read-only attributes first (Go's module cache is
/// read-only; Windows refuses to delete read-only files).
pub(crate) fn remove_tree(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if fs::remove_dir_all(dir).is_ok() {
        return Ok(());
    }
    clear_readonly(dir);
    fs::remove_dir_all(dir)
}

fn clear_readonly(dir: &Path) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        let mut perms = meta.permissions();
        if perms.readonly() {
            #[allow(clippy::permissions_set_readonly_false)] // deleting our own install tree
            perms.set_readonly(false);
            let _ = fs::set_permissions(&path, perms);
        }
        #[cfg(unix)]
        if meta.is_dir() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
        }
        // Symlinks were skipped above, so this never leaves `dir`.
        if meta.is_dir() {
            clear_readonly(&path);
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/install/archive.rs"]
mod tests;
