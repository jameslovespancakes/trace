//! Versioned binary cache files with checksums, atomic writes and a process lock.
//!
//! File layout (all integers little-endian):
//!
//! ```text
//! offset  size  field
//! 0       8     magic      (b"TRACEIDX" for the index, other 8-byte tags for side caches)
//! 8       4     schema     (u32; must equal the reader's expected version)
//! 12      4     reserved   (u32; 0)
//! 16      32    checksum   (blake3 of payload)
//! 48      8     length     (u64 payload byte length)
//! 56      n     payload    (postcard; the index payload is a sequence of length-prefixed
//!                            postcard blocks, see [`save_index`])
//! ```
//!
//! Any mismatch (magic, schema, length, checksum, decode, structural validation) is reported
//! as a cache error and the caller rebuilds; a corrupt cache is never partially trusted.
//!
//! Delta journal: an incremental update
//! appends a checked segment `index.bin.d<n>` (magic `TRACEJNL`, same envelope) instead of
//! rewriting the whole index. Its payload blocks are: a [`JournalMeta`] (checksum of the base
//! file it extends, sequence number, the paths it carries), the new index record (every part
//! but the per-file facts / semantics) and one `(facts, semantic)` block per carried file
//! (the files the delta added, modified or re-queried). [`load_index`] applies the segments
//! in order over the base (only the newest index record is decoded: each record replaces the
//! previous one); a segment that fails its checks is a cache error (full rebuild, never stale
//! data). Leftover segments of an older base (a crash between rewriting the base and
//! deleting them) are ignored. An update compacts the journal itself (the base rewritten,
//! the segments deleted) only when the carried file blocks would exceed 25% of the base, the
//! whole journal would exceed the base, or it would exceed 64 segments; the index record of
//! each segment is superseded by the next one, so it is not counted against the 25%. A
//! long-running process compacts earlier while idle ([`journal_wants_compaction`]), so an
//! edit never pays for rewriting the whole index (I-11).
//!
//! The build lock ([`CacheLock`]) is an OS file lock (`File::try_lock`): the operating
//! system releases it when the holder dies, so a crashed process never leaves a stale lock.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::facts::FileFacts;
use crate::fingerprint::Hash32;
use crate::model::Index;
use crate::semantics::FileSemantics;
use crate::SCHEMA_VERSION;

pub(crate) const INDEX_MAGIC: [u8; 8] = *b"TRACEIDX";
pub const SIMILAR_MAGIC: [u8; 8] = *b"TRACESIM";
/// Magic of a delta-journal segment (`index.bin.d<n>`).
pub(crate) const JOURNAL_MAGIC: [u8; 8] = *b"TRACEJNL";
/// Compaction: at most this many journal segments.
pub(crate) const MAX_JOURNAL_SEGMENTS: u32 = 64;
const HEADER_LEN: usize = 56;
/// Hard upper bound for any cache file (refuse to load larger). 4 GiB: indexes of very
/// large repositories (tensorflow/python) exceed 1 GiB of facts.
pub(crate) const MAX_CACHE_BYTES: u64 = 4 * 1_024 * 1_024 * 1_024;
/// Rename retries (Windows refuses to replace a file another process has open briefly).
const RENAME_ATTEMPTS: u32 = 8;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically: temp file in the same directory, fsync, rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_atomic_with(path, |f| f.write_all(bytes))
}

/// [`write_atomic`] with the content produced by `fill` (streamed through a buffered
/// writer, so large files never need one contiguous buffer).
pub(crate) fn write_atomic_with(
    path: &Path,
    fill: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| CoreError::Config(format!("no parent directory: {}", path.display())))?;
    fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
    let tmp = dir.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("cache"),
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let mut w = std::io::BufWriter::with_capacity(1 << 20, f);
        fill(&mut w)?;
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(CoreError::io(&tmp, e));
    }
    let mut attempt = 0;
    loop {
        match fs::rename(&tmp, path) {
            Ok(()) => break,
            // Sharing violations have no stable ErrorKind; retry anything but a missing file.
            Err(e)
                if attempt + 1 < RENAME_ATTEMPTS
                    && !matches!(e.kind(), ErrorKind::NotFound | ErrorKind::InvalidInput) =>
            {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(25 * u64::from(attempt)));
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(CoreError::io(path, e));
            }
        }
    }
    sync_dir(dir);
    Ok(())
}

/// Persist the rename itself (POSIX).
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Directory entries cannot be fsynced on this platform; the rename is atomic.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

/// Serialize `value` with postcard inside the checked envelope and write atomically.
pub fn save_blob<T: Serialize>(path: &Path, magic: [u8; 8], schema: u32, value: &T) -> Result<()> {
    save_payload(path, magic, schema, &encode(value)?)
}

fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    postcard::to_stdvec(value).map_err(|e| CoreError::Serialize(e.to_string()))
}

fn decode<T: DeserializeOwned>(bytes: &[u8], path: &Path) -> Result<T> {
    postcard::from_bytes(bytes).map_err(|e| CoreError::CacheCorrupt(format!("{}: {e}", path.display())))
}

/// Wrap `payload` in the checked envelope and write atomically.
fn save_payload(path: &Path, magic: [u8; 8], schema: u32, payload: &[u8]) -> Result<()> {
    if payload.len() as u64 + HEADER_LEN as u64 > MAX_CACHE_BYTES {
        return Err(CoreError::Limit(format!("{} would exceed the cache size limit", path.display())));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&magic);
    out.extend_from_slice(&schema.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&Hash32::of(payload).0);
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(payload);
    write_atomic(path, &out)
}

fn le_u32(bytes: &[u8]) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&bytes[..4]);
    u32::from_le_bytes(b)
}

fn le_u64(bytes: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(b)
}

/// Verify an envelope in memory and return the payload.
fn open_envelope<'b>(bytes: &'b [u8], path: &Path, magic: [u8; 8], schema: u32) -> Result<&'b [u8]> {
    if bytes.len() < HEADER_LEN || bytes[..8] != magic {
        return Err(CoreError::CacheCorrupt(format!("{}: bad header", path.display())));
    }
    let found = le_u32(&bytes[8..12]);
    if found != schema {
        return Err(CoreError::CacheVersion {
            found,
            expected: schema,
        });
    }
    let len = le_u64(&bytes[48..56]);
    let payload = &bytes[HEADER_LEN..];
    if payload.len() as u64 != len || Hash32::of(payload).0[..] != bytes[16..48] {
        return Err(CoreError::CacheCorrupt(format!("{}: checksum mismatch", path.display())));
    }
    Ok(payload)
}

/// Read a whole cache file (refusing files over [`MAX_CACHE_BYTES`]).
fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let mut file = File::open(path).map_err(|e| CoreError::io(path, e))?;
    let len = file.metadata().map_err(|e| CoreError::io(path, e))?.len();
    if len > MAX_CACHE_BYTES {
        return Err(CoreError::CacheCorrupt(format!("{} exceeds size limit", path.display())));
    }
    let mut bytes = Vec::with_capacity(len as usize);
    Read::by_ref(&mut file)
        .take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| CoreError::io(path, e))?;
    Ok(bytes)
}

/// Read and verify an envelope; decode the postcard payload.
pub fn load_blob<T: DeserializeOwned>(path: &Path, magic: [u8; 8], schema: u32) -> Result<T> {
    let bytes = read_bounded(path)?;
    decode(open_envelope(&bytes, path, magic, schema)?, path)
}

/// Write `blocks` as one checked envelope payload of length-prefixed blocks (streamed; the
/// checksum is computed over the same bytes). Returns the file size.
fn write_blocks(path: &Path, magic: [u8; 8], blocks: &[&[u8]]) -> Result<u64> {
    let len: u64 = blocks.iter().map(|b| 8 + b.len() as u64).sum();
    if len + HEADER_LEN as u64 > MAX_CACHE_BYTES {
        return Err(CoreError::Limit(format!("{} would exceed the cache size limit", path.display())));
    }
    let mut hasher = blake3::Hasher::new();
    for block in blocks {
        hasher.update(&(block.len() as u64).to_le_bytes());
        hasher.update(block);
    }
    let checksum = *hasher.finalize().as_bytes();
    write_atomic_with(path, |w| {
        w.write_all(&magic)?;
        w.write_all(&SCHEMA_VERSION.to_le_bytes())?;
        w.write_all(&0u32.to_le_bytes())?;
        w.write_all(&checksum)?;
        w.write_all(&len.to_le_bytes())?;
        for block in blocks {
            w.write_all(&(block.len() as u64).to_le_bytes())?;
            w.write_all(block)?;
        }
        Ok(())
    })?;
    Ok(len + HEADER_LEN as u64)
}

/// Persist the index. Payload = length-prefixed blocks (u64 LE length + postcard): the
/// index record first (per-file `facts` / `semantic` are not part of it), then one
/// `(facts, semantic)` block per file in `files` order. The per-file blocks hold most of the
/// bytes and are encoded and decoded in parallel.
///
/// The payload is streamed (checksum computed incrementally over the same bytes), so the
/// encoded blocks are the only copy of the index held in memory while saving. A new base
/// ends the delta journal: its segments are deleted afterwards (last first, so a crash in
/// between leaves only a prefix of leftover segments, which [`load_index`] ignores).
pub fn save_index(path: &Path, index: &Index) -> Result<()> {
    let head = encode(index)?;
    let blocks = index
        .files
        .par_iter()
        .map(|f| encode(&(&f.facts, &f.semantic)))
        .collect::<Result<Vec<_>>>()?;
    let all: Vec<&[u8]> = std::iter::once(head.as_slice())
        .chain(blocks.iter().map(Vec::as_slice))
        .collect();
    write_blocks(path, INDEX_MAGIC, &all)?;
    remove_journal(path)
}

/// First block of a journal segment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct JournalMeta {
    /// Checksum of the base file (`index.bin` envelope) this segment extends.
    pub base: [u8; 32],
    /// 1-based position in the journal.
    pub seq: u32,
    /// Files whose `(facts, semantic)` block follows the index record, in this order.
    pub paths: Vec<String>,
}

/// Path of journal segment `n` of the index at `path` (`index.bin.d<n>`).
pub(crate) fn journal_segment(path: &Path, n: u32) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "index.bin".to_string());
    path.with_file_name(format!("{name}.d{n}"))
}

/// Existing journal segments `1..=n` (contiguous) with their sizes.
fn journal_segments(path: &Path) -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    for n in 1..=MAX_JOURNAL_SEGMENTS + 1 {
        match fs::metadata(journal_segment(path, n)) {
            Ok(m) => out.push((n, m.len())),
            Err(_) => break,
        }
    }
    out
}

/// Delete the journal segments of `path`, last first.
fn remove_journal(path: &Path) -> Result<()> {
    for (n, _) in journal_segments(path).into_iter().rev() {
        let seg = journal_segment(path, n);
        match fs::remove_file(&seg) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(CoreError::io(&seg, e)),
        }
    }
    Ok(())
}

/// Checksum field of a cache envelope (reads the 56-byte header only); `None` when the file
/// is missing or not a `magic` envelope of this schema.
fn envelope_checksum(path: &Path, magic: [u8; 8]) -> Option<[u8; 32]> {
    let mut f = File::open(path).ok()?;
    let mut header = [0u8; HEADER_LEN];
    f.read_exact(&mut header).ok()?;
    if header[..8] != magic || le_u32(&header[8..12]) != SCHEMA_VERSION {
        return None;
    }
    let mut sum = [0u8; 32];
    sum.copy_from_slice(&header[16..48]);
    Some(sum)
}

/// Modification stamp of the persisted index: the newest of the base file and its journal
/// segments (a journal append changes the index without touching `index.bin`).
pub fn index_stamp(path: &Path) -> Option<SystemTime> {
    let base = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let newest = journal_segments(path)
        .into_iter()
        .filter_map(|(n, _)| fs::metadata(journal_segment(path, n)).and_then(|m| m.modified()).ok())
        .fold(base, |a, b| a.max(b));
    Some(newest)
}

/// Persist an incremental update: the new
/// index record plus the `(facts, semantic)` blocks of the files the delta added, modified
/// or re-queried, appended as the next checked journal segment (module docs). A full delta,
/// a missing or foreign base, or a journal over the compaction threshold rewrites the base
/// ([`save_index`]) instead.
pub fn save_index_delta(path: &Path, index: &Index, delta: &crate::delta::IndexDelta) -> Result<()> {
    if delta.full {
        return save_index(path, index);
    }
    let Some(base) = envelope_checksum(path, INDEX_MAGIC) else {
        return save_index(path, index);
    };
    let base_len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut segments = journal_segments(path);
    // Leftover segments of an older base: drop them before extending this base.
    if !segments.is_empty() && segment_base(&journal_segment(path, 1)) != Some(base) {
        remove_journal(path)?;
        segments.clear();
    }
    let carried: Vec<&crate::model::FileRecord> =
        index.files.iter().filter(|f| delta.file_changed(&f.path)).collect();
    let meta = JournalMeta {
        base,
        seq: segments.len() as u32 + 1,
        paths: carried.iter().map(|f| f.path.clone()).collect(),
    };
    let meta_block = encode(&meta)?;
    let head = encode(index)?;
    let blocks = carried
        .par_iter()
        .map(|f| encode(&(&f.facts, &f.semantic)))
        .collect::<Result<Vec<_>>>()?;
    let size: u64 = HEADER_LEN as u64
        + 16
        + meta_block.len() as u64
        + head.len() as u64
        + blocks.iter().map(|b| 8 + b.len() as u64).sum::<u64>();
    let journal: u64 = segments.iter().map(|(_, len)| *len).sum();
    // File blocks carried so far (each earlier segment also holds an index record about the
    // size of this one) plus this segment's.
    let head_len = (HEADER_LEN + 16 + meta_block.len() + head.len()) as u64;
    let carried_blocks = journal.saturating_sub(segments.len() as u64 * head_len)
        + blocks.iter().map(|b| 8 + b.len() as u64).sum::<u64>();
    let ratio = crate::config::current().cache.compact_ratio;
    if meta.seq > MAX_JOURNAL_SEGMENTS
        || carried_blocks.saturating_mul(ratio) > base_len
        || journal + size > base_len
    {
        return save_index(path, index);
    }
    let all: Vec<&[u8]> = [meta_block.as_slice(), head.as_slice()]
        .into_iter()
        .chain(blocks.iter().map(Vec::as_slice))
        .collect();
    write_blocks(&journal_segment(path, meta.seq), JOURNAL_MAGIC, &all)?;
    Ok(())
}

/// Whether an idle process should compact the journal of the index at `path` (rewrite the
/// base with [`save_index`]): the journal holds at least `cache.idle_compact_segments` segments
/// or `1 / cache.compact_ratio` of the base size. Reads file metadata only.
pub fn journal_wants_compaction(path: &Path) -> bool {
    let segments = journal_segments(path);
    if segments.is_empty() {
        return false;
    }
    let base_len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let journal: u64 = segments.iter().map(|(_, len)| *len).sum();
    let settings = &crate::config::current().cache;
    segments.len() >= settings.idle_compact_segments
        || journal.saturating_mul(settings.compact_ratio) > base_len
}

/// The base checksum a journal segment extends (`None`: unreadable or not a segment).
fn segment_base(segment: &Path) -> Option<[u8; 32]> {
    let bytes = read_bounded(segment).ok()?;
    let mut rest = open_envelope(&bytes, segment, JOURNAL_MAGIC, SCHEMA_VERSION).ok()?;
    let meta: JournalMeta = decode(next_block(&mut rest, segment).ok()?, segment).ok()?;
    Some(meta.base)
}

/// Split the next length-prefixed block off `rest`.
fn next_block<'b>(rest: &mut &'b [u8], path: &Path) -> Result<&'b [u8]> {
    let corrupt = || CoreError::CacheCorrupt(format!("{}: truncated index block", path.display()));
    if rest.len() < 8 {
        return Err(corrupt());
    }
    let len = usize::try_from(le_u64(rest)).map_err(|_| corrupt())?;
    let body = &rest[8..];
    if body.len() < len {
        return Err(corrupt());
    }
    let (block, tail) = body.split_at(len);
    *rest = tail;
    Ok(block)
}

/// Load the index (base plus delta journal), check it belongs to `expected_root` (display
/// form) and is structurally consistent ([`Index::validate`]).
pub fn load_index(path: &Path, expected_root: &str) -> Result<Index> {
    let bytes = read_bounded(path)?;
    let segment_bytes: Vec<(PathBuf, Vec<u8>)> = journal_segments(path)
        .into_iter()
        .map(|(n, _)| {
            let seg = journal_segment(path, n);
            read_bounded(&seg).map(|b| (seg, b))
        })
        .collect::<Result<Vec<_>>>()?;
    let base_id: [u8; 32] = {
        let mut sum = [0u8; 32];
        if bytes.len() >= HEADER_LEN {
            sum.copy_from_slice(&bytes[16..48]);
        }
        sum
    };
    let mut rest = open_envelope(&bytes, path, INDEX_MAGIC, SCHEMA_VERSION)?;
    let mut index: Index = decode(next_block(&mut rest, path)?, path)?;
    let mut blocks: std::collections::HashMap<String, &[u8]> =
        std::collections::HashMap::with_capacity(index.files.len());
    for f in &index.files {
        blocks.insert(f.path.clone(), next_block(&mut rest, path)?);
    }
    if !rest.is_empty() {
        return Err(CoreError::CacheCorrupt(format!(
            "{}: trailing bytes after the index blocks",
            path.display()
        )));
    }

    // Journal segments, in order. Every segment's index record replaces the previous one:
    // only the newest is decoded.
    let mut newest: Option<(&[u8], &Path)> = None;
    for (i, (seg, seg_bytes)) in segment_bytes.iter().enumerate() {
        let mut rest = open_envelope(seg_bytes, seg, JOURNAL_MAGIC, SCHEMA_VERSION)?;
        let meta: JournalMeta = decode(next_block(&mut rest, seg)?, seg)?;
        if meta.base != base_id {
            if i == 0 {
                // Leftovers of an older base (module docs): the base alone is current.
                break;
            }
            return Err(CoreError::CacheCorrupt(format!(
                "{}: journal segment of another index",
                seg.display()
            )));
        }
        if meta.seq as usize != i + 1 {
            return Err(CoreError::CacheCorrupt(format!("{}: journal segment out of order", seg.display())));
        }
        newest = Some((next_block(&mut rest, seg)?, seg.as_path()));
        for p in meta.paths {
            let block = next_block(&mut rest, seg)?;
            blocks.insert(p, block);
        }
        if !rest.is_empty() {
            return Err(CoreError::CacheCorrupt(format!(
                "{}: trailing bytes after the journal blocks",
                seg.display()
            )));
        }
    }
    if let Some((record, seg)) = newest {
        index = decode(record, seg)?;
    }

    if index.header.schema != SCHEMA_VERSION {
        return Err(CoreError::CacheVersion {
            found: index.header.schema,
            expected: SCHEMA_VERSION,
        });
    }
    if !same_root(&index.header.root, expected_root) {
        return Err(CoreError::CacheRoot(index.header.root));
    }
    let file_blocks = index
        .files
        .iter()
        .map(|f| {
            blocks.get(&f.path).copied().ok_or_else(|| {
                CoreError::CacheCorrupt(format!("{}: no block for {}", path.display(), f.path))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let per_file = file_blocks
        .par_iter()
        .map(|b| decode::<(Option<FileFacts>, Option<FileSemantics>)>(b, path))
        .collect::<Result<Vec<_>>>()?;
    for (file, (facts, semantic)) in index.files.iter_mut().zip(per_file) {
        file.facts = facts;
        file.semantic = semantic;
    }
    index.validate()?;
    Ok(index)
}

fn same_root(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// `<repo cache>/meta.json`: human-readable facts about a repository cache.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RepoMeta {
    pub root: String,
    pub created_unix: f64,
    pub last_index_unix: f64,
    pub schema: u32,
}

impl RepoMeta {
    /// Load `meta.json`; `None` if absent. Unparsable content is `CacheCorrupt`.
    pub fn load(path: &Path) -> Result<Option<RepoMeta>> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| CoreError::CacheCorrupt(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CoreError::io(path, e)),
        }
    }

    /// Record a completed index build (keeps `created_unix` of an existing, matching file).
    pub fn record_index(path: &Path, root: &str) -> Result<RepoMeta> {
        let now = unix_now();
        let created = match Self::load(path) {
            Ok(Some(m)) if same_root(&m.root, root) => m.created_unix,
            _ => now,
        };
        let meta = RepoMeta {
            root: root.to_string(),
            created_unix: created,
            last_index_unix: now,
            schema: SCHEMA_VERSION,
        };
        let bytes = serde_json::to_vec_pretty(&meta).map_err(|e| CoreError::Serialize(e.to_string()))?;
        write_atomic(path, &bytes)?;
        Ok(meta)
    }
}

/// Exclusive cross-process lock: an OS file lock (`File::try_lock`) on `path`, held while
/// this value lives and released by the operating system when the process dies (no stale
/// locks after a crash or a kill). The lock file itself stays (deleting it could let two
/// processes lock two different files of the same name); it holds the holder's pid for
/// diagnostics.
#[derive(Debug)]
pub struct CacheLock {
    path: PathBuf,
    /// The locked handle; the lock ends when it is closed.
    _file: File,
}

impl CacheLock {
    /// Acquire the lock or fail with [`CoreError::Locked`] (another process, or another
    /// handle of this process, holds it).
    pub fn acquire(path: &Path) -> Result<CacheLock> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| CoreError::io(path, e))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(CoreError::Locked(path.to_path_buf()));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(CoreError::io(path, e)),
        }
        // Diagnostics only: the pid and time of the holder.
        let _ = file.set_len(0);
        let _ = writeln!(file, "{} {}", std::process::id(), unix_now() as u64);
        let _ = file.flush();
        Ok(CacheLock {
            path: path.to_path_buf(),
            _file: file,
        })
    }

    /// Whether some process currently holds the lock at `path` (status reporting only).
    pub(crate) fn is_held(path: &Path) -> bool {
        let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
            return false;
        };
        match file.try_lock() {
            Ok(()) => {
                let _ = file.unlock();
                false
            }
            Err(std::fs::TryLockError::WouldBlock) => true,
            Err(std::fs::TryLockError::Error(_)) => false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Seconds since the UNIX epoch as f64 (index timestamps).
pub fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
#[path = "../tests/unit/cache.rs"]
mod tests;
