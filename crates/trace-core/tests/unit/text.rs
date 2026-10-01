use super::*;

#[test]
fn crlf_bom_and_utf16_round_trip() {
    let src = "\u{FEFF}a = '\u{1F600}'\r\nb = 1\rc\n".as_bytes();
    let idx = LineIndex::new(src);
    assert_eq!(idx.line_count(), 4);
    // After the emoji (2 UTF-16 units) on line 0: "a = '" is 5 units, emoji 2 => col 7.
    let byte = idx.byte_of_utf16(src, 0, 7).unwrap();
    assert_eq!(idx.utf16_of_byte(src, byte), (0, 7));
    assert!(idx.byte_of_utf16(src, 0, 6).is_err());
    assert_eq!(idx.line_text(src, 1), "b = 1");
    assert_eq!(idx.line_text(src, 2), "c");
    assert_eq!(idx.line1(idx.byte_of_utf16(src, 2, 0).unwrap()), 3);
}

#[test]
fn bom_is_not_a_column_and_columns_clamp() {
    let src = "\u{FEFF}ab\ncd".as_bytes();
    let idx = LineIndex::new(src);
    assert!(idx.has_bom());
    // Column 0 on line 0 is the first byte after the BOM.
    assert_eq!(idx.byte_of_utf16(src, 0, 0).unwrap(), 3);
    assert_eq!(idx.utf16_of_byte(src, 4), (0, 1));
    // Past the line end clamps to the end of the line (before the terminator).
    assert_eq!(idx.byte_of_utf16(src, 0, 99).unwrap(), 5);
    assert_eq!(idx.byte_of_utf16(src, 1, 99).unwrap(), 8);
    assert!(idx.byte_of_utf16(src, 5, 0).is_err());
    assert_eq!(idx.line_text(src, 0), "ab");
}

#[test]
fn terminators_and_multibyte() {
    // `\r\n`, lone `\r` and `\n` all end lines; a trailing terminator opens an empty line.
    let src = "a\r\n\rb\n".as_bytes();
    let idx = LineIndex::new(src);
    assert_eq!(idx.line_count(), 4);
    assert_eq!(idx.line_text(src, 1), "");
    assert_eq!(idx.line_text(src, 2), "b");
    assert_eq!(idx.line_text(src, 3), "");
    assert_eq!(idx.line1(0), 1);
    assert_eq!(idx.line1(4), 3);
    assert_eq!(idx.line1(99), 4);

    // "é" is 2 bytes / 1 unit; "𝄞" is 4 bytes / 2 units.
    let src = "é𝄞x".as_bytes();
    let idx = LineIndex::new(src);
    assert_eq!(idx.byte_of_utf16(src, 0, 1).unwrap(), 2);
    assert_eq!(idx.byte_of_utf16(src, 0, 3).unwrap(), 6);
    assert!(idx.byte_of_utf16(src, 0, 2).is_err());
    assert_eq!(idx.utf16_of_byte(src, 6), (0, 3));

    // Invalid UTF-8 counts one unit per byte and never panics.
    let src = b"a\xFFb";
    let idx = LineIndex::new(src);
    assert_eq!(idx.byte_of_utf16(src, 0, 2).unwrap(), 2);
    assert_eq!(idx.utf16_of_byte(src, 3), (0, 3));
}
