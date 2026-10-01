use super::*;

#[test]
fn rule_go_file_name_constraints_select_the_host_platform() {
    let (os, arch) = host();
    assert!(built_on_host("exec.go"));
    assert!(built_on_host("exec_posix.go"), "`posix` is no GOOS: no constraint");
    assert!(built_on_host(&format!("exec_{os}.go")));
    assert!(built_on_host(&format!("zsys_{os}_{arch}.go")));
    assert!(built_on_host(&format!("exec_{os}_test.go")));
    let other_os = if os == "plan9" { "windows" } else { "plan9" };
    assert!(!built_on_host(&format!("exec_{other_os}.go")));
    assert!(!built_on_host(&format!("zsys_{other_os}_{arch}.go")));
    let other_arch = if arch == "s390x" { "amd64" } else { "s390x" };
    assert!(!built_on_host(&format!("asm_{other_arch}.go")));
    // The first element is the file's own name, never a constraint.
    assert!(built_on_host(&format!("{other_os}.go")));
}
