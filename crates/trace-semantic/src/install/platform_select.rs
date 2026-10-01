//! Artifact selection for the current platform (owner install): archive artifacts by
//! `Platform::key()`, npm packages by their `os` / `cpu` fields, R binaries by platform,
//! Linux distribution and R minor.

use trace_env::os::{Arch, Os, Platform, Version};

use crate::registry::{Artifact, NpmPackage, RBinary};

/// The artifact for `platform` (exact `Platform::key()` first, then "any"). A musl Linux never
/// gets a glibc build.
pub(crate) fn artifact_for<'a>(artifacts: &'a [Artifact], platform: &Platform) -> Option<&'a Artifact> {
    let key = platform.key();
    artifacts
        .iter()
        .find(|a| a.platform == key)
        .or_else(|| artifacts.iter().find(|a| a.platform == "any"))
}

/// Why no usable artifact exists here, as the second line of `ServerUnavailable` (None when
/// there is simply no build for this platform).
pub(crate) fn unavailable_advice(artifacts: &[Artifact], platform: &Platform) -> Option<String> {
    if platform.os == Os::Linux && platform.musl {
        let glibc_key = Platform {
            musl: false,
            ..platform.clone()
        }
        .key();
        if artifacts.iter().any(|a| a.platform == glibc_key) {
            return Some(
                "It needs a Linux with the GNU C library (glibc); this system uses musl.".to_string(),
            );
        }
    }
    None
}

/// The glibc an artifact needs when this Linux has an older one (None = fine).
pub(crate) fn glibc_too_old(artifact: &Artifact, glibc: Option<&Version>) -> Option<String> {
    let need = Version::parse(artifact.min_glibc.as_deref()?)?;
    match glibc {
        Some(have) if *have >= need => None,
        // Unknown glibc (no libc found where expected): let the program decide at launch.
        None => None,
        Some(_) => Some(format!("It needs Linux with glibc {} or newer.", need.text)),
    }
}

/// npm `os` value of this platform.
pub(crate) fn npm_os(platform: &Platform) -> &'static str {
    match platform.os {
        Os::Windows => "win32",
        Os::Linux => "linux",
        Os::MacOs => "darwin",
    }
}

/// npm `cpu` value of this platform.
pub(crate) fn npm_cpu(platform: &Platform) -> String {
    match platform.arch {
        Arch::X86_64 => "x64".to_string(),
        Arch::Aarch64 => "arm64".to_string(),
        Arch::Other => platform.arch_name.clone(),
    }
}

/// npm's rule for `os` / `cpu` lists: empty = any; `!x` entries exclude; otherwise the value
/// must be listed.
fn npm_list_allows(list: &[String], value: &str) -> bool {
    if list.is_empty() {
        return true;
    }
    if list.iter().any(|v| v.strip_prefix('!') == Some(value)) {
        return false;
    }
    let positive: Vec<&String> = list.iter().filter(|v| !v.starts_with('!')).collect();
    positive.is_empty() || positive.iter().any(|v| v.as_str() == value)
}

/// Whether an npm package of the lock is installed on `platform` (foreign-platform packages,
/// such as another OS's native TypeScript binary or macOS-only `fsevents`, are skipped).
pub(crate) fn npm_package_applies(package: &NpmPackage, platform: &Platform) -> bool {
    npm_list_allows(&package.os, npm_os(platform)) && npm_list_allows(&package.cpu, &npm_cpu(platform))
}

/// "x.y" of an R version.
pub fn r_minor(version: &Version) -> String {
    format!("{}.{}", version.parts.first().copied().unwrap_or(0), version.parts.get(1).copied().unwrap_or(0))
}

/// The pinned R binaries for this platform, Linux distribution and R minor, in the recorded
/// (dependency) order. Empty when nothing is pinned for this combination.
pub(crate) fn r_binaries_for<'a>(
    files: &'a [RBinary],
    platform: &Platform,
    distro: Option<&str>,
    r_minor: &str,
) -> Vec<&'a RBinary> {
    let key = platform.key();
    files
        .iter()
        .filter(|f| f.platform == key && f.r_minor == r_minor)
        .filter(|f| match (&f.distro, distro) {
            (None, _) => platform.os != Os::Linux,
            (Some(want), Some(have)) => want.eq_ignore_ascii_case(have),
            (Some(_), None) => false,
        })
        .collect()
}

/// The R minors pinned for this platform / distribution (for the unavailable advice).
pub(crate) fn r_minors_for(files: &[RBinary], platform: &Platform, distro: Option<&str>) -> Vec<String> {
    let key = platform.key();
    let mut minors: Vec<String> = files
        .iter()
        .filter(|f| f.platform == key)
        .filter(|f| match (&f.distro, distro) {
            (None, _) => true,
            (Some(want), Some(have)) => want.eq_ignore_ascii_case(have),
            (Some(_), None) => false,
        })
        .map(|f| f.r_minor.clone())
        .collect();
    minors.sort_by(|a, b| match (Version::parse(a), Version::parse(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => a.cmp(b),
    });
    minors.dedup();
    minors
}

#[cfg(test)]
#[path = "../../tests/unit/install/platform_select.rs"]
mod tests;
