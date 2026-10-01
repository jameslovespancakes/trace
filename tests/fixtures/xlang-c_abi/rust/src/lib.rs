// Fixture (P5, bridge kind c_abi): Rust <-> C through unmangled C symbol names.

/// Unique: declared by c/api.h, defined only here -> proven c_abi from the C prototype.
#[no_mangle]
pub extern "C" fn rs_add(a: i32, b: i32) -> i32 {
    a + b
}

extern "C" {
    /// Unique: defined once in c/compress.c -> proven.
    fn c_compress(x: i32) -> i32;
    /// Ambiguous: defined in c/dup_a.c and c/dup_b.c -> two possible rows.
    fn c_dup(x: i32) -> i32;
    /// Negative control: no definition in the repository -> no bridge.
    fn c_missing(x: i32) -> i32;
}

pub fn use_all(x: i32) -> i32 {
    unsafe { c_compress(x) + c_dup(x) + c_missing(x) }
}

/// Negative control: same name as a C function but not exported (no #[no_mangle]).
pub fn local_only(x: i32) -> i32 {
    x
}
