//! Downloads (owner install): https only (redirects only to https), a size cap, streamed to
//! disk while hashing, verified (sha256 hex or an npm SRI sha512) before anything uses the
//! bytes, content-addressed cache `<tools>/downloads/<digest>` (a verified cached file is
//! reused; a damaged one is deleted).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256, Sha512};

/// Largest artifact a download may have in bytes (setting `semantic.max_download_mb`).
fn max_download_bytes() -> u64 {
    trace_core::config::current().semantic.max_download_mb << 20
}

/// The expected digest of a download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expected {
    /// Lower-case hex sha256 (install records, R binaries, jars).
    Sha256(String),
    /// npm Subresource Integrity (`sha512-<base64>`; other algorithms are ignored).
    Sri(String),
}

impl Expected {
    /// File name in the download cache.
    fn cache_name(&self) -> Option<String> {
        match self {
            Expected::Sha256(hex) => {
                let hex = hex.trim().to_ascii_lowercase();
                (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hex)
            }
            Expected::Sri(sri) => {
                let raw = decode_base64(sri_sha512(sri)?)?;
                (raw.len() == 64).then(|| format!("sha512-{}", hex(&raw)))
            }
        }
    }
}

/// Why a download failed.
#[derive(Debug)]
pub enum FetchError {
    /// Network / HTTP / size problem (details for the log).
    Download(String),
    /// The bytes do not match the pinned digest.
    Checksum {
        url: String,
        expected: String,
        got: String,
    },
    /// The record carries no usable digest (a registry defect).
    NoDigest(String),
    Io(std::io::Error),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Download(m) => write!(f, "download failed: {m}"),
            FetchError::Checksum { url, expected, got } => {
                write!(f, "checksum mismatch for {url}: expected {expected}, got {got}")
            }
            FetchError::NoDigest(url) => write!(f, "no pinned digest for {url}"),
            FetchError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for FetchError {
    fn from(e: std::io::Error) -> Self {
        FetchError::Io(e)
    }
}

/// Hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Hex sha256 of a file (streamed).
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The base64 part of the first `sha512-` entry of an SRI string.
fn sri_sha512(sri: &str) -> Option<&str> {
    sri.split_whitespace()
        .find_map(|part| part.strip_prefix("sha512-"))
        .map(|b64| b64.split('?').next().unwrap_or(b64))
}

/// Standard base64 (with or without padding) -> bytes.
pub(crate) fn decode_base64(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let body = text.trim().trim_end_matches('=');
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for c in body.bytes() {
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Whether `bytes` match the SRI `integrity` (sha512).
pub(crate) fn sri_matches(integrity: &str, bytes: &[u8]) -> bool {
    sri_sha512(integrity)
        .and_then(decode_base64)
        .is_some_and(|want| want.as_slice() == Sha512::digest(bytes).as_slice())
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .https_only(true)
        .redirects(8)
        .timeout_connect(std::time::Duration::from_secs(30))
        .timeout_read(std::time::Duration::from_secs(120))
        .user_agent(concat!("trace-installer/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Largest metadata answer [`fetch`] reads (bytes).
pub(crate) const MAX_METADATA_BYTES: u64 = 4 << 20;

/// A small https metadata document (a release-selection API answer) read into memory. The
/// artifacts such an answer names are downloaded with `download_verified` against the
/// digests it lists.
pub fn fetch(url: &str) -> Result<Vec<u8>, FetchError> {
    if !url.starts_with("https://") {
        return Err(FetchError::Download(format!("refusing non-https url {url}")));
    }
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| FetchError::Download(format!("{url}: {e}")))?;
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut body)?;
    if body.len() as u64 > MAX_METADATA_BYTES {
        return Err(FetchError::Download(format!("{url}: larger than {MAX_METADATA_BYTES} bytes")));
    }
    Ok(body)
}

/// Download `url` into `<tools>/downloads/<digest>` (reused when already there and valid),
/// streaming to a `.part` file while hashing; verify the digest; return the verified file.
/// `on_bytes(done, total)` reports progress.
pub(crate) fn download_verified(
    tools_dir: &Path,
    url: &str,
    expected: &Expected,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, FetchError> {
    if !url.starts_with("https://") {
        return Err(FetchError::Download(format!("refusing non-https url {url}")));
    }
    let name = expected
        .cache_name()
        .ok_or_else(|| FetchError::NoDigest(url.to_string()))?;
    if let Some(path) = cached_verified(tools_dir, expected)? {
        return Ok(path);
    }
    let dir = tools_dir.join("downloads");
    let path = dir.join(&name);
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| FetchError::Download(format!("{url}: {e}")))?;
    let total = response
        .header("Content-Length")
        .and_then(|v| v.trim().parse::<u64>().ok());
    let max = max_download_bytes();
    if total.is_some_and(|t| t > max) {
        return Err(FetchError::Download(format!("{url}: larger than {max} bytes")));
    }
    let part = dir.join(format!("{name}.part-{}", uuid::Uuid::new_v4().simple()));
    let result = stream_to(&part, response.into_reader(), url, total, on_bytes);
    let (sha256, sha512) = match result {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    let (ok, got, want) = match expected {
        Expected::Sha256(h) => (sha256 == h.trim().to_ascii_lowercase(), sha256.clone(), h.clone()),
        Expected::Sri(sri) => {
            let want = sri_sha512(sri).and_then(decode_base64).unwrap_or_default();
            (want == sha512, hex(&sha512), sri.clone())
        }
    };
    if !ok {
        let _ = std::fs::remove_file(&part);
        return Err(FetchError::Checksum {
            url: url.to_string(),
            expected: want,
            got,
        });
    }
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    std::fs::rename(&part, &path)?;
    Ok(path)
}

/// The verified cached download for `expected` (`<tools>/downloads/<digest>`); a cached file
/// that does not verify is deleted (never returned).
pub(crate) fn cached_verified(tools_dir: &Path, expected: &Expected) -> Result<Option<PathBuf>, FetchError> {
    let Some(name) = expected.cache_name() else {
        return Ok(None);
    };
    let dir = tools_dir.join("downloads");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    if !path.is_file() {
        return Ok(None);
    }
    if verify_file(&path, expected)? {
        return Ok(Some(path));
    }
    let _ = std::fs::remove_file(&path);
    Ok(None)
}

/// Stream `reader` into `part` with the size cap; returns (sha256 hex, sha512 bytes).
fn stream_to(
    part: &Path,
    reader: impl Read,
    url: &str,
    total: Option<u64>,
    on_bytes: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(String, Vec<u8>), FetchError> {
    let max = max_download_bytes();
    let mut reader = reader.take(max + 1);
    let mut file = std::io::BufWriter::new(std::fs::File::create(part)?);
    let mut s256 = Sha256::new();
    let mut s512 = Sha512::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut done: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| FetchError::Download(format!("{url}: {e}")))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > max {
            return Err(FetchError::Download(format!("{url}: larger than {max} bytes")));
        }
        s256.update(&buf[..n]);
        s512.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        on_bytes(done, total);
    }
    file.flush()?;
    drop(file);
    Ok((hex(&s256.finalize()), s512.finalize().to_vec()))
}

/// Verify a file on disk against `expected`.
pub(crate) fn verify_file(path: &Path, expected: &Expected) -> Result<bool, FetchError> {
    match expected {
        Expected::Sha256(h) => Ok(sha256_file(path)? == h.trim().to_ascii_lowercase()),
        Expected::Sri(sri) => {
            let bytes = std::fs::read(path)?;
            Ok(sri_matches(sri, &bytes))
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/install/fetch.rs"]
mod tests;
