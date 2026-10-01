use super::*;

fn write(dir: &Path, rel: &str, text: &str) -> PathBuf {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    std::fs::write(&path, text).expect("write");
    path
}

/// The members of a library class are its own methods, then those of its bases found
/// from its source: same file, relative imports, re-exports; the first declaration wins.
#[test]
fn rule_library_class_members_follow_its_bases() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "lib/__init__.py", "from .base import Base\n");
    write(
        dir.path(),
        "lib/base.py",
        "class Base:\n    def open(self):\n        pass\n\n    def get(self):\n        pass\n",
    );
    let client = write(
        dir.path(),
        "lib/client.py",
        "from . import Base\n\nclass Mixin:\n    def close(self):\n        pass\n\nclass Client(Mixin, Base):\n    def open(self):\n        pass\n",
    );
    let class = class_at(&FsLoader, Language::Python, &client, 6, 6, &[]).expect("class");
    assert_eq!(class.symbol, "lib.client.Client");
    let members: Vec<(&str, &str)> = class.members.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(
        members,
        vec![
            ("close", "lib.client.Mixin.close"),
            ("get", "lib.base.Base.get"),
            ("open", "lib.client.Client.open"),
        ]
    );
    assert!(class_at(&FsLoader, Language::Python, &client, 0, 0, &[]).is_none(), "an import, not a type");
}

/// Python data model: `__getattribute__` runs for every member access, so the members
/// of such a class are not known.
#[test]
fn rule_library_class_with_attribute_hook_has_no_known_members() {
    let dir = tempfile::tempdir().expect("tempdir");
    let proxy = write(
        dir.path(),
        "proxy.py",
        "class Base:\n    def __getattribute__(self, name):\n        pass\n\nclass Proxy(Base):\n    def get(self):\n        pass\n",
    );
    assert!(class_at(&FsLoader, Language::Python, &proxy, 4, 6, &[]).is_none());
}
