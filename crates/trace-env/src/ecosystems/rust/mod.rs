//! Rust: toolchain (rustup or a plain install), Cargo projects, dependencies.
//!
//! Everything here reads files; the only execution is `rustc -vV` on a toolchain binary
//! when no channel manifest names the version ([`crate::os::toolchain_output`]).
//!
//! * **Toolchain** ([`resolve`]): `--env <toolchain root>` -> rustup (project pin
//!   `rust-toolchain.toml` / `rust-toolchain` from the root upward, then a `rustup override`
//!   directory entry of `<RUSTUP_HOME>/settings.toml`, then `RUSTUP_TOOLCHAIN`, then
//!   `default_toolchain`) -> a non-rustup `cargo` + `rustc` on PATH -> standard install
//!   locations. The version comes from `<root>/lib/rustlib/multirust-channel-manifest.toml`
//!   (`[pkg.rustc] version`). Facts: `sysroot`, `host`, `rustup_toolchain`, `cargo_home`,
//!   `rustup_home`, `rust_src` (the standard library source, when installed), `channel`,
//!   `pin`. A project `rust-version` (MSRV) above the toolchain is `TooOld`.
//! * **Cargo projects** ([`cargo_layout`]): the manifests between each `.rs` file and the
//!   root; workspace roots and standalone packages that are not nested inside another project
//!   are required, the others are sub-projects. Workspace membership follows Cargo's rules
//!   (`members` / `exclude` globs, path dependencies inside the workspace directory).
//! * **Dependencies** ([`deps`]): every registry package of `Cargo.lock` that the host needs
//!   must be in the Cargo home (`registry/cache/*/<name>-<version>.crate` or
//!   `registry/src/*/<name>-<version>`) or in the vendored source directory of
//!   `.cargo/config.toml`. Crates declared only for other platforms (`[target.'cfg(..)']`
//!   tables evaluated against the host triple) are not required. The authoritative check is
//!   `cargo metadata --offline` under the build approval (`trace_semantic` `languages/rust.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use trace_core::Language;

use crate::os::{self, EnvVars, Os, Platform, Version, VersionReq};
use crate::relpath;
use crate::{
    entries, subdirs, DepsReport, DepsStatus, DetectContext, EcosystemId, LibraryKind, LibraryRoot, Origin,
    SubProject, Toolchain, ToolchainStatus,
};

mod cargo;
mod cfg;
mod deps;
mod toolchain;

pub use cargo::*;
use cfg::*;
pub use deps::*;
pub use toolchain::*;

/// Dependency hint of the Rust ecosystem.
pub const DEPS_HINT: &str = "cargo fetch";

/// Upper bound of manifests read per repository (bounded work).
const MAX_MANIFESTS: usize = 2_000;

/// The Rust ecosystem ([`crate::Ecosystem`]).
pub struct Rust;

impl crate::Ecosystem for Rust {
    fn id(&self) -> crate::EcosystemId {
        crate::EcosystemId::Rust
    }

    fn accepts_env_path(&self, path: &Path) -> bool {
        accepts_env_path(path)
    }

    fn toolchain(&self, cx: &DetectContext<'_>) -> ToolchainStatus {
        toolchain(cx)
    }

    fn deps(&self, read: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
        deps(read, toolchain)
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/ecosystems/rust/mod.rs"]
mod tests;
