//! blake3 fingerprints for sources, inventories, evidence and tool bundles.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A 32-byte blake3 digest. Serialized as raw bytes (postcard); rendered as lowercase hex.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub struct Hash32(pub [u8; 32]);

impl Hash32 {
    /// Digest of a byte slice.
    pub fn of(bytes: &[u8]) -> Self {
        Hash32(*blake3::hash(bytes).as_bytes())
    }

    /// Full lowercase hex (64 chars).
    pub fn to_hex(&self) -> String {
        blake3::Hash::from(self.0).to_hex().to_string()
    }

    /// First 12 hex chars, for display only.
    pub fn short(&self) -> String {
        let mut hex = self.to_hex();
        hex.truncate(12);
        hex
    }

    /// First `n` hex chars (n <= 64), used for compact stable keys (site ids, repo keys).
    pub fn hex_prefix(&self, n: usize) -> String {
        let mut hex = self.to_hex();
        hex.truncate(n.min(64));
        hex
    }
}

impl fmt::Debug for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash32({})", self.short())
    }
}

impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Incremental hasher over length-prefixed parts, so `["ab","c"]` != `["a","bc"]`.
#[derive(Default)]
pub struct PartsHasher(blake3::Hasher);

impl PartsHasher {
    pub fn new() -> Self {
        PartsHasher(blake3::Hasher::new())
    }

    /// Append one part (length-prefixed).
    pub fn part(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.update(&(bytes.len() as u64).to_le_bytes());
        self.0.update(bytes);
        self
    }

    /// Append a string part.
    pub fn text(&mut self, s: &str) -> &mut Self {
        self.part(s.as_bytes())
    }

    /// Append an integer part.
    pub fn int(&mut self, v: u64) -> &mut Self {
        self.part(&v.to_le_bytes())
    }

    pub fn finish(&self) -> Hash32 {
        Hash32(*self.0.finalize().as_bytes())
    }
}

/// Canonical JSON (serde_json default map = sorted keys, compact) hashed with blake3.
/// Used for evidence packets and site keys; stable across runs and platforms.
pub fn hash_json(value: &serde_json::Value) -> Hash32 {
    // serde_json::to_vec on a Value cannot fail.
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    Hash32::of(&bytes)
}

#[cfg(test)]
#[path = "../tests/unit/fingerprint.rs"]
mod tests;
