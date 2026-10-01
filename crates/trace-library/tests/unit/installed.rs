use super::*;

fn root(path: &Path, ecosystem: EcosystemId, layout: &'static str) -> LibraryRoot {
    LibraryRoot {
        path: path.to_path_buf(),
        kind: LibraryKind::Dependency,
        ecosystem,
        layout,
        version: None,
    }
}

#[test]
fn rule_installed_packages_from_library_roots() {
    let dir = tempfile::tempdir().expect("tempdir");
    let site = dir.path().join("site-packages");
    std::fs::create_dir_all(site.join("Web_Kit-3.0.0.dist-info")).unwrap();
    std::fs::create_dir_all(site.join("web_kit")).unwrap();
    let nm = dir.path().join("node_modules");
    std::fs::create_dir_all(nm.join("router-kit")).unwrap();
    std::fs::create_dir_all(nm.join("@scope").join("pkg")).unwrap();
    std::fs::create_dir_all(nm.join(".bin")).unwrap();
    let modcache = dir.path().join("mod");
    std::fs::create_dir_all(modcache.join("example.com").join("!acme").join("lib@v1.2.0")).unwrap();
    let installed = InstalledPackages::from_roots(&[
        root(&site, EcosystemId::Python, "site_packages"),
        root(&nm, EcosystemId::Node, "node_modules"),
        root(&modcache, EcosystemId::Go, "go_modcache"),
    ]);
    assert!(installed.contains(EcosystemId::Python, "web-kit"));
    assert!(installed.contains(EcosystemId::Python, "Web.Kit"));
    assert!(!installed.contains(EcosystemId::Python, "other-kit"));
    assert!(installed.contains(EcosystemId::Node, "router-kit"));
    assert!(installed.contains(EcosystemId::Node, "@scope/pkg"));
    assert!(!installed.contains(EcosystemId::Node, ".bin"));
    assert!(installed.contains(EcosystemId::Go, "example.com/Acme/lib"));
    assert!(!installed.contains(EcosystemId::Php, "router-kit"), "per ecosystem");
    // Standard-library roots are not dependencies.
    let mut std_root = root(&site, EcosystemId::Python, "site_packages");
    std_root.kind = LibraryKind::Stdlib;
    assert!(!InstalledPackages::from_roots(&[std_root]).contains(EcosystemId::Python, "web-kit"));
}
