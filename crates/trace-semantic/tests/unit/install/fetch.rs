use super::*;

fn temp() -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-fetch-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Downloads are verified by sha256 before use: a cached file with the right digest is
/// reused without the network, a damaged cached file is never returned, https only, and a
/// record without a digest is refused.
#[test]
fn rule_installer_verifies_sha256() {
    assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    let tools = temp();
    let good = Expected::Sha256(sha256_hex(b"payload"));
    std::fs::create_dir_all(tools.join("downloads")).unwrap();
    let cached = tools.join("downloads").join(sha256_hex(b"payload"));
    std::fs::write(&cached, b"payload").unwrap();
    let mut seen = |_: u64, _: Option<u64>| {};
    // The url is never contacted: the cached file verifies.
    let got = download_verified(&tools, "https://invalid.invalid/x", &good, &mut seen).unwrap();
    assert_eq!(got, cached);
    // A damaged cached file is deleted, never returned (download_verified then fetches).
    std::fs::write(&cached, b"tampered").unwrap();
    assert_eq!(cached_verified(&tools, &good).unwrap(), None);
    assert!(!cached.exists(), "a damaged download is never kept");
    assert!(matches!(
        download_verified(&tools, "http://example.com/x", &good, &mut seen),
        Err(FetchError::Download(_))
    ));
    assert!(matches!(
        download_verified(&tools, "https://example.com/x", &Expected::Sha256(String::new()), &mut seen),
        Err(FetchError::NoDigest(_))
    ));
    assert!(verify_file(&tools.join("nope"), &good).is_err());
    let _ = std::fs::remove_dir_all(&tools);
}

/// npm tarballs are verified against their SRI sha512 (base64), other algorithms ignored.
#[test]
fn rule_installer_verifies_npm_integrity() {
    let bytes = b"tarball bytes";
    let digest = Sha512::digest(bytes);
    let b64 = encode_base64(&digest);
    let sri = format!("sha1-AAAA sha512-{b64}");
    assert!(sri_matches(&sri, bytes));
    assert!(!sri_matches(&sri, b"other bytes"));
    assert!(!sri_matches("sha1-AAAA", bytes), "only sha512 counts");
    assert_eq!(decode_base64("YWJj"), Some(b"abc".to_vec()));
    assert_eq!(decode_base64("YWI="), Some(b"ab".to_vec()));
    assert_eq!(decode_base64("Y*"), None);
    assert_eq!(Expected::Sri(sri).cache_name(), Some(format!("sha512-{}", hex(&digest))));
}

fn encode_base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
