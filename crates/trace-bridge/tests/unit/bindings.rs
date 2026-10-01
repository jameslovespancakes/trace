use super::*;

#[test]
fn cargo_names() {
    let toml = "[workspace]\nmembers=[]\n[package]\nname = \"wasm-game-of-life\"\nversion=\"0.1.0\"\n";
    assert_eq!(cargo_package_name(toml).as_deref(), Some("wasm-game-of-life"));
    assert_eq!(cargo_package_name("[dependencies]\nname = \"x\"\n"), None);
}
