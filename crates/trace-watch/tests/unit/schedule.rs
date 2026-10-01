use super::*;

#[test]
fn rule_changed_paths_are_repository_relative() {
    let root = Path::new("/repo");
    assert_eq!(relative(root, Path::new("/repo/src/a.py")).as_deref(), Some("src/a.py"));
    assert_eq!(relative(root, Path::new("/elsewhere/a.py")), None);
    assert_eq!(relative(root, Path::new("/repo")), None);
}
