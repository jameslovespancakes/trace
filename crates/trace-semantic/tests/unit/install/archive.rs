use super::*;
use trace_env::os::Arch;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join(format!("trace-fixtures-archive-{name}-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A tar entry written byte-for-byte (names the builder would refuse are allowed here).
fn tar_entry(
    out: &mut tar::Builder<Vec<u8>>,
    name: &str,
    kind: tar::EntryType,
    mode: u32,
    data: &[u8],
    link: &str,
) {
    let mut h = tar::Header::new_ustar();
    let bytes = name.as_bytes();
    h.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
    h.set_entry_type(kind);
    h.set_mode(mode);
    h.set_size(data.len() as u64);
    if !link.is_empty() {
        h.set_link_name_literal(link).unwrap();
    }
    h.set_cksum();
    out.append(&h, data).unwrap();
}

fn tar_bytes(entries: &[(&str, tar::EntryType, u32, &[u8], &str)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, kind, mode, data, link) in entries {
        tar_entry(&mut b, name, *kind, *mode, data, link);
    }
    b.into_inner().unwrap()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    for (name, data) in entries {
        w.start_file(*name, zip::write::SimpleFileOptions::default().unix_permissions(0o755))
            .unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn platform() -> Platform {
    Platform::current()
}

#[test]
fn rule_installer_detects_format_by_magic_bytes() {
    assert_eq!(detect(b"PK\x03\x04rest"), Format::Zip);
    assert_eq!(detect(&[0x1f, 0x8b, 8]), Format::Gzip);
    assert_eq!(detect(&[0xfd, b'7', b'z', b'X', b'Z', 0, 1]), Format::Xz);
    assert_eq!(detect(b"MZ"), Format::Raw);
    let dir = temp("formats");
    let tar = tar_bytes(&[("pkg/bin/tool", tar::EntryType::Regular, 0o755, b"tool", "")]);
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut io::Cursor::new(tar.clone()), &mut xz).unwrap();
    // The file names lie on purpose: the magic bytes decide.
    let cases: Vec<(&str, Vec<u8>, Format)> = vec![
        ("a.sit", zip_bytes(&[("pkg/bin/tool", b"tool")]), Format::Zip),
        ("a.zip", gzip(&tar), Format::Gzip),
        ("a.bin", xz, Format::Xz),
    ];
    for (name, bytes, format) in cases {
        let file = dir.join(name);
        fs::write(&file, bytes).unwrap();
        let out = dir.join(format!("out-{name}"));
        fs::create_dir_all(&out).unwrap();
        let opts = ExtractOptions {
            strip: 1,
            ..Default::default()
        };
        assert_eq!(extract(&file, &out, &opts, &mut Budget::default()).unwrap(), format);
        assert_eq!(fs::read(out.join("bin").join("tool")).unwrap(), b"tool", "{name}");
    }
    // A single gzip-compressed file (rust-analyzer) and a raw binary (Expert).
    let p = platform();
    let single = single_file_name("bin/rust-analyzer", &p);
    for (name, bytes, format) in [
        ("ra.gz", gzip(b"ELF-binary"), Format::Gzip),
        ("raw", b"ELF-binary".to_vec(), Format::Raw),
    ] {
        let file = dir.join(name);
        fs::write(&file, bytes).unwrap();
        let out = dir.join(format!("single-{name}"));
        fs::create_dir_all(&out).unwrap();
        let opts = ExtractOptions {
            single_file: Some(&single),
            ..Default::default()
        };
        assert_eq!(extract(&file, &out, &opts, &mut Budget::default()).unwrap(), format);
        let exe = executable_path(&out, "bin/rust-analyzer", &p).unwrap();
        assert_eq!(fs::read(exe).unwrap(), b"ELF-binary");
    }
    let _ = fs::remove_dir_all(&dir);
}

/// Absolute paths, `..`, drive prefixes, device names, escaping symlinks and hard links
/// make the whole archive refused.
#[test]
fn rule_installer_rejects_path_traversal() {
    for bad in [
        "../evil",
        "a/../../evil",
        "/etc/passwd",
        "C:/x",
        "a/CON",
        "a/nul.txt",
        "a/b:stream",
    ] {
        assert!(matches!(sanitize(bad), Err(ArchiveError::Unsafe(_))), "{bad}");
    }
    assert_eq!(sanitize("./pkg//bin/x").unwrap(), vec!["pkg", "bin", "x"]);
    let dir = temp("traversal");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("dotdot.tgz", gzip(&tar_bytes(&[("pkg/../../evil", tar::EntryType::Regular, 0o644, b"x", "")]))),
        ("abs.tgz", gzip(&tar_bytes(&[("/tmp/evil", tar::EntryType::Regular, 0o644, b"x", "")]))),
        ("link.tgz", gzip(&tar_bytes(&[("pkg/l", tar::EntryType::Symlink, 0o777, b"", "../../outside")]))),
        ("hard.tgz", gzip(&tar_bytes(&[("pkg/h", tar::EntryType::Link, 0o644, b"", "pkg/x")]))),
        ("dev.tgz", gzip(&tar_bytes(&[("pkg/d", tar::EntryType::Char, 0o644, b"", "")]))),
        ("zipslip.zip", zip_bytes(&[("../evil", b"x")])),
    ];
    for (name, bytes) in cases {
        let file = dir.join(name);
        fs::write(&file, bytes).unwrap();
        let out = dir.join(format!("out-{name}"));
        fs::create_dir_all(&out).unwrap();
        let err = extract(&file, &out, &ExtractOptions::default(), &mut Budget::default());
        assert!(matches!(err, Err(ArchiveError::Unsafe(_))), "{name}: {err:?}");
    }
    assert!(!dir.join("evil").exists() && !dir.parent().unwrap().join("evil").exists());
    // A symlink that stays inside is fine.
    assert!(link_stays_inside(&["bin".into(), "npm".into()], "../lib/npm-cli.js"));
    assert!(!link_stays_inside(&["npm".into()], "../x"));
    // The unpacked-size cap.
    let file = dir.join("big.tgz");
    fs::write(&file, gzip(&tar_bytes(&[("pkg/f", tar::EntryType::Regular, 0o644, &[0u8; 4096], "")])))
        .unwrap();
    let out = dir.join("out-big");
    fs::create_dir_all(&out).unwrap();
    let mut small = Budget { remaining: 100 };
    assert!(matches!(
        extract(&file, &out, &ExtractOptions::default(), &mut small),
        Err(ArchiveError::TooLarge)
    ));
    let _ = fs::remove_dir_all(&dir);
}

/// macOS JDKs keep only `jdk-<v>/Contents/Home/`; nupkgs keep only their `subdir`.
#[test]
fn rule_installer_strip_prefix_for_macos_jdk() {
    let dir = temp("prefix");
    let tar = tar_bytes(&[
        ("jdk-21.0.12.1+1/Contents/Home/bin/java", tar::EntryType::Regular, 0o755, b"java", ""),
        ("jdk-21.0.12.1+1/Contents/Home/release", tar::EntryType::Regular, 0o644, b"JAVA_VERSION=\"21\"", ""),
        ("jdk-21.0.12.1+1/Contents/Info.plist", tar::EntryType::Regular, 0o644, b"plist", ""),
    ]);
    let file = dir.join("jdk.tar.gz");
    fs::write(&file, gzip(&tar)).unwrap();
    let out = dir.join("jdk");
    fs::create_dir_all(&out).unwrap();
    let opts = ExtractOptions {
        strip_prefix: Some("jdk-21.0.12.1+1/Contents/Home/"),
        ..Default::default()
    };
    extract(&file, &out, &opts, &mut Budget::default()).unwrap();
    assert!(out.join("bin").join("java").is_file());
    assert!(out.join("release").is_file());
    assert!(!out.join("Info.plist").exists() && !out.join("Contents").exists());
    let nupkg = dir.join("roslyn.nupkg");
    fs::write(
        &nupkg,
        zip_bytes(&[
            ("tools/net10.0/win-x64/Server.dll", b"dll"),
            ("_rels/.rels", b"x"),
            ("tools/net10.0/linux-x64/Server.dll", b"other"),
        ]),
    )
    .unwrap();
    let out = dir.join("roslyn");
    fs::create_dir_all(&out).unwrap();
    let opts = ExtractOptions {
        subdir: Some("tools/net10.0/win-x64"),
        ..Default::default()
    };
    extract(&nupkg, &out, &opts, &mut Budget::default()).unwrap();
    assert_eq!(fs::read(out.join("Server.dll")).unwrap(), b"dll");
    assert!(!out.join("_rels").exists() && !out.join("tools").exists());
    let _ = fs::remove_dir_all(&dir);
}

/// Declared executables get exec bits on Unix even when the archive says 0644; on Windows
/// `bin/x` resolves to `bin/x.exe`.
#[test]
fn rule_installer_sets_exec_bits_on_unix() {
    let dir = temp("exec");
    let p = platform();
    let name = single_file_name("bin/tool", &p);
    let tar = tar_bytes(&[(&format!("pkg/{name}"), tar::EntryType::Regular, 0o644, b"x", "")]);
    let file = dir.join("t.tgz");
    fs::write(&file, gzip(&tar)).unwrap();
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    extract(
        &file,
        &out,
        &ExtractOptions {
            strip: 1,
            ..Default::default()
        },
        &mut Budget::default(),
    )
    .unwrap();
    let found = mark_executables(&out, &["bin/tool".to_string(), "bin/missing".to_string()], &p).unwrap();
    assert_eq!(found.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&found[0].1).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "exec bits set");
    }
    let win = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
        arch_name: "x86_64".into(),
        musl: false,
    };
    assert_eq!(single_file_name("bin/tool", &win), "bin/tool.exe");
    assert_eq!(single_file_name("cs.jar", &win), "cs.jar");
    let _ = fs::remove_dir_all(&dir);
}
