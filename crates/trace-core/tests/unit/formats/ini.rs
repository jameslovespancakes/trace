use super::*;

#[test]
fn rule_key_value_files_are_read_structurally() {
    let kv = key_values(
        "# comment\nJAVA_VERSION=\"21.0.12\"\nhome = C:\\Python312\n\nversion_info = 3.12.4\nbad line\nX='y'\n",
    );
    assert_eq!(kv["JAVA_VERSION"], "21.0.12");
    assert_eq!(kv["home"], "C:\\Python312");
    assert_eq!(kv["version_info"], "3.12.4");
    assert_eq!(kv["X"], "y");
    assert_eq!(kv.len(), 4);
}

#[test]
fn rule_ini_sections_join_continuation_lines() {
    let ini = sections("[options]\ninstall_requires =\n    numpy\n    scipy>=1\n[metadata]\nname = pkg\n");
    assert_eq!(ini["options"]["install_requires"].trim(), "numpy\nscipy>=1");
    assert_eq!(ini["metadata"]["name"], "pkg");
}
