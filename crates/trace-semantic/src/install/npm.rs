//! npm packages from an embedded lock (owner install): every tarball is downloaded (8 in
//! parallel), verified against its SRI sha512 and unpacked (its top directory, usually
//! `package/`, stripped) into `<dest>/<path>`; packages for another OS / CPU are skipped; the
//! files a package declares as `bin` get exec bits (what npm's linker does). No npm, no user
//! Node, never a package script.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use trace_env::os::Platform;

use super::archive::{self, Budget, ExtractOptions};
use super::fetch::{self, Expected, FetchError};
use super::platform_select::npm_package_applies;
use super::StepError;
use crate::registry::NpmPackage;

/// The packages of the lock installed on `platform`, in lock order.
pub fn selected<'a>(packages: &'a [NpmPackage], platform: &Platform) -> Vec<&'a NpmPackage> {
    packages.iter().filter(|p| npm_package_applies(p, platform)).collect()
}

/// Install the lock into `dest` (a staging directory). `on_progress(done, total)` counts
/// downloaded packages. Returns the downloaded files (for cache cleanup).
pub(crate) fn install(
    tools_dir: &Path,
    packages: &[NpmPackage],
    dest: &Path,
    platform: &Platform,
    budget: &mut Budget,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<PathBuf>, StepError> {
    let chosen = selected(packages, platform);
    let files = download_all(tools_dir, &chosen, on_progress)?;
    for (pkg, file) in chosen.iter().zip(&files) {
        let target = dest.join(
            crate::registry::safe_relative(&pkg.path)
                .ok_or_else(|| StepError::Failed(format!("npm path {:?} is not relative", pkg.path)))?,
        );
        std::fs::create_dir_all(&target)?;
        let opts = ExtractOptions {
            strip: 1,
            ..Default::default()
        };
        archive::extract(file, &target, &opts, budget)?;
        mark_bins(&target)?;
    }
    Ok(files)
}

/// Download every package (bounded parallelism), in lock order.
fn download_all(
    tools_dir: &Path,
    chosen: &[&NpmPackage],
    on_progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<PathBuf>, StepError> {
    let total = chosen.len() as u64;
    let next = AtomicUsize::new(0);
    let mut results: Vec<Option<Result<PathBuf, FetchError>>> = (0..chosen.len()).map(|_| None).collect();
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<(usize, Result<PathBuf, FetchError>)>();
        let parallel = trace_core::config::current().semantic.parallel_downloads.max(1);
        for _ in 0..parallel.min(chosen.len().max(1)) {
            let tx = tx.clone();
            let next = &next;
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(pkg) = chosen.get(i) else { break };
                let got = fetch::download_verified(
                    tools_dir,
                    &pkg.url,
                    &Expected::Sri(pkg.integrity.clone()),
                    &mut |_, _| {},
                );
                let failed = got.is_err();
                if tx.send((i, got)).is_err() || failed {
                    // Stop taking new work after a failure; the others finish theirs.
                    next.store(usize::MAX / 2, Ordering::SeqCst);
                }
            });
        }
        drop(tx);
        let mut done = 0u64;
        for (i, got) in rx {
            done += 1;
            on_progress(done, total);
            if let Some(slot) = results.get_mut(i) {
                *slot = Some(got);
            }
        }
    });
    let mut files = Vec::with_capacity(results.len());
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Some(Ok(path)) => files.push(path),
            Some(Err(e)) => return Err(e.into()),
            None => {
                return Err(StepError::Download(format!(
                    "{} was not downloaded",
                    chosen.get(i).map(|p| p.url.as_str()).unwrap_or("?")
                )))
            }
        }
    }
    Ok(files)
}

/// Exec bits for the files a package declares in `package.json` `bin` (a string, or a map of
/// command name -> relative path).
fn mark_bins(package_dir: &Path) -> Result<(), StepError> {
    let Ok(bytes) = std::fs::read(package_dir.join("package.json")) else {
        return Ok(());
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Ok(());
    };
    let bins: Vec<&str> = match json.get("bin") {
        Some(serde_json::Value::String(s)) => vec![s.as_str()],
        Some(serde_json::Value::Object(map)) => map.values().filter_map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    };
    for rel in bins {
        let Ok(parts) = archive::sanitize(rel) else {
            continue;
        };
        let mut path = package_dir.to_path_buf();
        for p in parts {
            path.push(p);
        }
        if path.is_file() {
            archive::set_executable(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/install/npm.rs"]
mod tests;
