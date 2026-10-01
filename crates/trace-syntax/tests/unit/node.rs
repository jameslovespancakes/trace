use super::*;

#[test]
fn truncation_respects_char_boundaries() {
    let s = "aé".repeat(10);
    let t = truncate_bytes(s, 4);
    assert!(t.len() <= 4);
    assert!(t.is_char_boundary(t.len()));
    assert_eq!(truncate_bytes("abc".into(), 10), "abc");
}

#[test]
fn slice_clamps() {
    assert_eq!(slice(b"hello", 3, 99), "lo");
    assert_eq!(slice(b"hello", 9, 99), "");
}
