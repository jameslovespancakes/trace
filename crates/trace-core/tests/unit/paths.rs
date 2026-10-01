use super::*;
use crate::test_support::Fixture;

#[test]
fn lexical_helpers() {
    assert_eq!(normalize_lexically(Path::new("/a/b/../c/./d")), PathBuf::from("/a/c/d"));
    assert_eq!(normalize_lexically(Path::new("../../x")), PathBuf::from("../../x"));
    assert_eq!(normalize_lexically(Path::new("a/../../x")), PathBuf::from("../x"));
    assert_eq!(normalize_lexically(Path::new("/..")), PathBuf::from("/"));
    assert!(key_within("c:/x/repo/sub", "c:/x/repo"));
    assert!(key_within("c:/x/repo", "c:/x/repo"));
    assert!(!key_within("c:/x/repo2", "c:/x/repo"));
    assert!(is_within(Path::new("/srv/a/b"), Path::new("/srv/a/")));
    assert!(!is_within(Path::new("/srv/ab"), Path::new("/srv/a")));
}

#[cfg(windows)]
#[test]
fn windows_keys_are_case_insensitive() {
    assert!(is_within(Path::new(r"C:\Users\X\Repo\src"), Path::new("c:/users/x/repo")));
    assert_eq!(path_key(Path::new(r"\\?\C:\A\")), "c:/a");
}

#[test]
fn forbidden_roots_are_matched_lexically() {
    let fake = std::env::temp_dir().join("trace-not-a-real-forbidden-root");
    assert!(ensure_not_forbidden(&fake).is_ok());
    assert!(forbidden_keys_below(Path::new("/definitely/elsewhere")).is_empty());
}

#[test]
fn rule_watching_means_the_watch_lock_is_held() {
    let fx = Fixture::new("paths-watch");
    let root = fx.dir("repo");
    let home = fx.dir("home");
    let paths = RepoPaths::resolve_in(&root, &home).unwrap();
    assert!(!paths.watching());
    let lock = crate::cache::CacheLock::acquire(&paths.watch_lock_file()).unwrap();
    assert!(paths.watching());
    drop(lock);
    assert!(!paths.watching(), "a stopped watcher leaves no trace");
}

#[test]
fn cache_home_inside_root_is_refused() {
    let fx = Fixture::new("paths");
    let root = fx.dir("repo");
    let inside = root.join("cache");
    assert!(matches!(RepoPaths::resolve_in(&root, &inside), Err(CoreError::InsideInspectedRoot(_))));
    let home = fx.dir("home");
    let paths = RepoPaths::resolve_in(&root, &home).unwrap();
    assert_eq!(paths.key.len(), 16);
    assert!(paths.index_file.starts_with(&home));
    assert_eq!(paths.key, repo_key(&paths.root));
    assert!(ensure_outside(&paths.index_file, &[&paths.root]).is_ok());
    assert!(ensure_outside(&paths.root.join("x/y.bin"), &[&paths.root]).is_err());

    paths.ensure_repo_dir().unwrap();
    assert!(paths.repo_dir.is_dir());
}
