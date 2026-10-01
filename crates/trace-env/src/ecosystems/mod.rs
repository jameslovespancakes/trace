//! The ecosystems: one module per toolchain / dependency ecosystem, each with a unit struct
//! implementing [`Ecosystem`] (`python::Python`, `node::Node`, ...), and the registry
//! ([`EcosystemId`], [`EcosystemId::ecosystem`]). Adding an ecosystem = one module here with
//! its `impl Ecosystem` + one [`EcosystemId`] variant.

use std::path::Path;

use serde::{Deserialize, Serialize};
use trace_core::Language;

use crate::{DepsReport, DetectContext, Ecosystem, Toolchain, ToolchainStatus};

pub mod cfamily;
pub mod dotnet;
pub mod go;
pub mod haskell;
pub mod jvm;
pub mod node;
pub mod php;
pub mod python;
pub mod r;
pub mod rust;

/// A toolchain/dependency ecosystem. `as_str()` is the key of `RepoSettings::env`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EcosystemId {
    Python,
    Node,
    Php,
    Rust,
    Go,
    #[serde(rename = "cfamily")]
    CFamily,
    Jvm,
    Dotnet,
    Haskell,
    R,
}

impl EcosystemId {
    pub const ALL: [EcosystemId; 10] = [
        EcosystemId::Python,
        EcosystemId::Node,
        EcosystemId::Php,
        EcosystemId::Rust,
        EcosystemId::Go,
        EcosystemId::CFamily,
        EcosystemId::Jvm,
        EcosystemId::Dotnet,
        EcosystemId::Haskell,
        EcosystemId::R,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            EcosystemId::Python => "python",
            EcosystemId::Node => "node",
            EcosystemId::Php => "php",
            EcosystemId::Rust => "rust",
            EcosystemId::Go => "go",
            EcosystemId::CFamily => "cfamily",
            EcosystemId::Jvm => "jvm",
            EcosystemId::Dotnet => "dotnet",
            EcosystemId::Haskell => "haskell",
            EcosystemId::R => "r",
        }
    }

    pub fn parse(s: &str) -> Option<EcosystemId> {
        EcosystemId::ALL
            .iter()
            .copied()
            .find(|e| e.as_str().eq_ignore_ascii_case(s.trim()))
    }

    /// The ecosystem of a code language (Bash and non-code languages: None).
    pub const fn of_language(language: Language) -> Option<EcosystemId> {
        Some(match language {
            Language::Python => EcosystemId::Python,
            Language::JavaScript | Language::TypeScript | Language::Tsx => EcosystemId::Node,
            Language::Php => EcosystemId::Php,
            Language::Rust => EcosystemId::Rust,
            Language::Go => EcosystemId::Go,
            Language::C | Language::Cpp => EcosystemId::CFamily,
            Language::Java | Language::Scala => EcosystemId::Jvm,
            Language::CSharp => EcosystemId::Dotnet,
            Language::Haskell => EcosystemId::Haskell,
            Language::R => EcosystemId::R,
            _ => return None,
        })
    }

    /// Basenames of the files that declare a repository's dependencies in this ecosystem (an
    /// environment is where those are installed; none for C / C++ and .NET).
    pub const fn manifests(self) -> &'static [&'static str] {
        match self {
            EcosystemId::Python => &[
                "requirements.txt",
                "pyproject.toml",
                "setup.py",
                "setup.cfg",
                "Pipfile",
                "poetry.lock",
                "uv.lock",
            ],
            EcosystemId::Node => &["package.json"],
            EcosystemId::Php => &["composer.json"],
            EcosystemId::Rust => &["Cargo.toml"],
            EcosystemId::Go => &["go.mod"],
            EcosystemId::Jvm => &["pom.xml", "build.gradle", "build.gradle.kts", "build.sbt"],
            EcosystemId::R => &["DESCRIPTION"],
            EcosystemId::Haskell => &["cabal.project", "stack.yaml"],
            EcosystemId::CFamily | EcosystemId::Dotnet => &[],
        }
    }

    /// [`Ecosystem::accepts_env_path`] of this ecosystem.
    pub(crate) fn accepts_env_path(self, path: &Path) -> bool {
        self.ecosystem().accepts_env_path(path)
    }

    /// [`Ecosystem::toolchain`] of this ecosystem.
    pub fn toolchain(self, cx: &DetectContext<'_>) -> ToolchainStatus {
        self.ecosystem().toolchain(cx)
    }

    /// [`Ecosystem::deps`] of this ecosystem.
    pub fn deps(self, cx: &DetectContext<'_>, toolchain: Option<&Toolchain>) -> DepsReport {
        self.ecosystem().deps(cx, toolchain)
    }

    /// The implementation of this ecosystem.
    pub fn ecosystem(self) -> &'static dyn Ecosystem {
        match self {
            EcosystemId::Python => &python::Python,
            EcosystemId::Node => &node::Node,
            EcosystemId::Php => &php::Php,
            EcosystemId::Rust => &rust::Rust,
            EcosystemId::Go => &go::Go,
            EcosystemId::CFamily => &cfamily::CFamily,
            EcosystemId::Jvm => &jvm::Jvm,
            EcosystemId::Dotnet => &dotnet::Dotnet,
            EcosystemId::Haskell => &haskell::Haskell,
            EcosystemId::R => &r::R,
        }
    }
}

/// The ecosystem that accepts `path` as its environment (`--env`), in `EcosystemId::ALL` order.
pub fn classify_env_path(path: &Path) -> Option<EcosystemId> {
    EcosystemId::ALL.iter().copied().find(|e| e.accepts_env_path(path))
}

#[cfg(test)]
#[path = "../../tests/unit/ecosystems/mod.rs"]
mod tests;
