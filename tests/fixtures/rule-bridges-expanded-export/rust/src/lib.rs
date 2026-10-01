// Fixture (bridges, expanded export): an attribute macro whose expansion exports the
// function under its C name (delivered by the language server's macro expansion).
#[export_fn]
pub fn add_numbers(a: i32, b: i32) -> i32 {
    a + b
}

pub fn private_helper() -> i32 {
    0
}
