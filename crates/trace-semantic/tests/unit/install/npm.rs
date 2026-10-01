use super::*;
use sha2::Digest;
use std::io::Write;

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, data) in files {
        let mut h = tar::Header::new_ustar();
        h.set_path(name).unwrap();
        h.set_mode(0o644);
        h.set_size(data.len() as u64);
        h.set_cksum();
        b.append(&h, *data).unwrap();
    }
    let tar = b.into_inner().unwrap();
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(&tar).unwrap();
    e.finish().unwrap()
}

/// A lock installs from verified cached tarballs (no network): the top directory is
/// stripped, scoped paths land under node_modules/@scope, foreign-platform packages are
/// skipped, `bin` files are marked executable.
#[test]
fn rule_npm_lock_installs_verified_tarballs_without_scripts() {
    let root = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-npm-{}", uuid::Uuid::new_v4().simple()));
    let tools = root.join("tools");
    std::fs::create_dir_all(tools.join("downloads")).unwrap();
    let platform = Platform::current();
    let mut packages = Vec::new();
    for (path, top, foreign) in [
        ("node_modules/pyright", "package", false),
        ("node_modules/@types/node", "node", false),
        ("node_modules/fsevents", "package", true),
    ] {
        let bytes = tgz(&[
            (
                &format!("{top}/package.json"),
                br#"{"name":"x","bin":{"x":"dist/cli.js"},"scripts":{"postinstall":"evil"}}"#,
            ),
            (&format!("{top}/dist/cli.js"), b"console.log(1)"),
        ]);
        let digest = sha2::Sha512::digest(&bytes);
        let integrity = format!("sha512-{}", b64(&digest));
        let cache = tools
            .join("downloads")
            .join(format!("sha512-{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>()));
        std::fs::write(cache, &bytes).unwrap();
        let other_os = if super::super::platform_select::npm_os(&platform) == "darwin" {
            "win32"
        } else {
            "darwin"
        };
        packages.push(NpmPackage {
            path: path.into(),
            version: "1.0.0".into(),
            url: format!("https://registry.invalid/{path}.tgz"),
            integrity,
            os: if foreign {
                vec![other_os.into()]
            } else {
                Vec::new()
            },
            cpu: Vec::new(),
            optional: foreign,
        });
    }
    let dest = root.join("stage");
    std::fs::create_dir_all(&dest).unwrap();
    let mut seen = Vec::new();
    let files =
        install(&tools, &packages, &dest, &platform, &mut Budget::default(), &mut |d, t| seen.push((d, t)))
            .unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(seen.last(), Some(&(2, 2)));
    assert!(dest.join("node_modules/pyright/dist/cli.js").is_file());
    assert!(dest.join("node_modules/@types/node/package.json").is_file());
    assert!(!dest.join("node_modules/fsevents").exists(), "foreign platform skipped");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.join("node_modules/pyright/dist/cli.js"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
    let _ = std::fs::remove_dir_all(&root);
}
