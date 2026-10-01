use super::*;

#[test]
fn hex_round_trip_and_prefixes() {
    let h = Hash32::of(b"trace");
    let hex = h.to_hex();
    assert_eq!(hex.len(), 64);
    assert_eq!(h.short(), &hex[..12]);
    assert_eq!(h.hex_prefix(20), &hex[..20]);
    assert_eq!(h.hex_prefix(100), hex);
    assert_eq!(h.to_string(), hex);
}

#[test]
fn parts_are_length_prefixed() {
    let parts = |ps: &[&str]| {
        let mut h = PartsHasher::new();
        for p in ps {
            h.text(p);
        }
        h.finish()
    };
    assert_ne!(parts(&["ab", "c"]), parts(&["a", "bc"]));
    assert_eq!(parts(&["a", "b"]), parts(&["a", "b"]));
    let mut h = PartsHasher::new();
    h.text("x").int(7);
    assert_ne!(h.finish(), parts(&["x"]));
}

#[test]
fn json_hash_uses_sorted_compact_keys() {
    let a: serde_json::Value = serde_json::from_str(r#"{"b": 1, "a": [1, 2]}"#).unwrap();
    let b: serde_json::Value = serde_json::from_str(r#"{"a":[1,2],"b":1}"#).unwrap();
    assert_eq!(hash_json(&a), hash_json(&b));
    assert_eq!(hash_json(&a), Hash32::of(br#"{"a":[1,2],"b":1}"#));
}
