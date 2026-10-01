//! Go setup hooks (owner native): gopls built for the user's Go minor, the user's module
//! cache read-only, cgo only under the build approval.
//!
//! **Preflight** (collects every independent failure):
//! 1. toolchain (`trace_env::go::toolchain`): Go missing, or older than the `go` directive;
//! 2. server: the gopls version for the Go minor (registry `go_install` versions: Go >= 1.26 ->
//!    v0.23.0, 1.25 -> v0.21.1, 1.24.2+ -> v0.20.0; older Go cannot build any supported gopls)
//!    must be the installed one (`trace status --install go` builds it with the user's Go);
//! 3. dependencies: every `require` in the module cache (`go mod download`);
//! 4. cgo: files importing `"C"` (syntax facts) run the C compiler over their preambles ->
//!    approval, then a gcc/clang (cgo does not use MSVC).
//!
//! **Prepared env**: `GOROOT`, PATH = `<GOROOT>/bin` (+ the cgo compiler) + the user's PATH,
//! `GOMODCACHE`, `GOPATH` (first entry), `CGO_ENABLED` / `CC`; the registry adds
//! `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local GOENV=off GOTELEMETRY=off` and `GOCACHE` outside
//! the mirror. No `GOFLAGS`, no `GOWORK` override.
//!
//! **outside_build**: gopls answers "no package metadata" / "no packages found" for files that
//! build constraints exclude on this machine (custom build tags, other GOOS/GOARCH): those
//! files are outside the build, their calls unknown - not a setup failure.

use std::path::Path;

use trace_core::fingerprint::PartsHasher;
use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::os::{Os, Version, VersionReq};
use trace_env::Ecosystem as _;
use trace_env::EcosystemId;

use super::{default_prepared, Prepared, Server, SetupContext};
use super::{detect_context, toolchain_spec};
use crate::backends::fntype::FnTypeRoute;
use crate::registry::{BuildSpec, BuildWhen, GoInstallVersion, Recipe};
use crate::setup::{deps_error, require_approval, require_server, toolchain_error, Collect};
use trace_env::lookup::compose_path;

pub struct Hooks;

/// The gopls release the user's Go can build and run: the entry with the highest `min_go`
/// that `go` satisfies.
pub fn gopls_for_go<'v>(versions: &'v [GoInstallVersion], go: &Version) -> Option<&'v GoInstallVersion> {
    let mut sorted: Vec<(&GoInstallVersion, Version)> = versions
        .iter()
        .filter_map(|v| Some((v, Version::parse(&v.min_go)?)))
        .collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
        .into_iter()
        .find(|(_, min)| VersionReq::at_least(min.clone()).matches(go))
        .map(|(v, _)| v)
}

/// Go files that import `"C"` (cgo), from the syntax facts.
fn cgo_files<'a>(cx: &SetupContext<'a>) -> Vec<&'a str> {
    cx.files
        .iter()
        .filter(|(_, l)| *l == Language::Go)
        .filter(|(p, _)| (cx.facts)(p).is_some_and(|f| f.imports.iter().any(|i| i.target == "C")))
        .map(|(p, _)| *p)
        .collect()
}

/// (needs, install) for a missing cgo compiler on this OS.
fn cgo_compiler_advice(os: Os) -> (&'static str, &'static str) {
    match os {
        Os::Windows => (
            "a C compiler for cgo (MinGW-w64 gcc)",
            "Install MinGW-w64, for example with MSYS2 from https://www.msys2.org,",
        ),
        Os::MacOs => {
            ("a C compiler for cgo", "Install the Xcode Command Line Tools (xcode-select --install)")
        }
        Os::Linux => ("a C compiler for cgo", "Install gcc or clang with your package manager"),
    }
}

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = Language::Go;
        let mut collect = Collect::default();
        let dcx = detect_context(cx, EcosystemId::Go);
        let spec = toolchain_spec(cx, "go", "Go", "Install it from https://go.dev/dl");
        // The Go toolchain is searched through the execute context; go.mod / go.work / go.sum
        // and the module cache are only read (the repository included: `readable`).
        let readable = trace_core::paths::forbidden_roots();
        let env = trace_env::go::Go.detect(&dcx, &readable);
        if let Some(e) = toolchain_error(language, &spec, &env.toolchain) {
            collect.push(e);
        }
        let toolchain = env.toolchain.value().cloned();

        // Server: gopls for this Go minor.
        let install = cx.entry.install.as_ref();
        let versions: &[GoInstallVersion] = match install.map(|i| &i.recipe) {
            Some(Recipe::GoInstall { versions, .. }) => versions,
            _ => &[],
        };
        let mut gopls: Option<&GoInstallVersion> = None;
        match toolchain.as_ref().and_then(|t| t.version.clone()) {
            Some(go) if !versions.is_empty() => match gopls_for_go(versions, &go) {
                Some(g) => {
                    gopls = Some(g);
                    let id = install.map(|i| i.id.as_str()).unwrap_or("gopls");
                    let installed = cx.tools.manifest.tools.get(id).map(|m| m.version.as_str());
                    if installed != Some(g.version.as_str()) || cx.tools.tool_dir(id).is_none() {
                        collect.push(SetupError::ServerMissing { language });
                    }
                }
                None => {
                    let oldest = versions
                        .iter()
                        .filter_map(|v| Version::parse(&v.min_go))
                        .min()
                        .map(|v| v.text)
                        .unwrap_or_default();
                    collect.push(SetupError::ToolchainVersion {
                        language,
                        needs: format!("Go {oldest} or newer"),
                        source: "the Go language server".into(),
                        tool: "Go".into(),
                        found: go.text.clone(),
                        install: spec.install.clone(),
                    });
                }
            },
            _ => {
                collect.check(require_server(cx));
            }
        }

        let deps = env.dependencies;
        if let Some(e) = deps_error(language, &deps) {
            collect.push(e);
        }

        let cgo = cgo_files(cx);
        let mut cgo_compiler = None;
        if !cgo.is_empty() {
            let build = BuildSpec {
                tool: "cgo".into(),
                runs: "the C compiler on its code".into(),
                when: BuildWhen::DecidedByHooks,
            };
            if collect.check(require_approval(cx, &build)).is_some() {
                cgo_compiler = trace_env::cfamily::cgo_compiler(cx.vars, cx.platform);
                if cgo_compiler.is_none() {
                    let (needs, install) = cgo_compiler_advice(cx.platform.os);
                    collect.push(SetupError::ToolchainMissing {
                        language,
                        needs: needs.into(),
                        install: install.into(),
                    });
                }
            }
        }

        let Some(toolchain) = toolchain.filter(|_| collect.is_empty()) else {
            return collect.finish(default_prepared(cx));
        };
        let mut prepared = default_prepared(cx);
        let goroot = toolchain.root.display().to_string();
        prepared.vars.insert("toolchain".into(), goroot.clone());
        prepared.env.insert("GOROOT".into(), goroot.clone());
        let mut path_dirs = vec![toolchain.root.join("bin")];
        if let Some(c) = &cgo_compiler {
            path_dirs.extend(c.cc.parent().map(Path::to_path_buf));
            prepared.env.insert("CC".into(), c.cc.display().to_string());
        }
        prepared
            .env
            .insert("PATH".into(), compose_path(&path_dirs, cx.vars, cx.platform));
        for key in ["GOMODCACHE", "GOPATH"] {
            if let Some(v) = toolchain.facts.get(key) {
                prepared.env.insert(key.into(), v.clone());
            }
        }
        // A module cache only the read context sees (inside the repository: nothing there is
        // executed, but gopls reads it).
        if !prepared.env.contains_key("GOMODCACHE") {
            if let Some(cache) = deps.roots.iter().find(|r| r.layout == "go_modcache") {
                prepared
                    .env
                    .insert("GOMODCACHE".into(), cache.path.display().to_string());
            }
        }
        let cgo_on = cgo_compiler.is_some();
        prepared
            .env
            .insert("CGO_ENABLED".into(), if cgo_on { "1" } else { "0" }.into());
        prepared.runs_project_code = cgo_on;
        prepared.library_roots = deps.roots.clone();
        let go_version = toolchain
            .version
            .as_ref()
            .map(|v| v.text.clone())
            .unwrap_or_else(|| "unknown".into());
        let mut fp = PartsHasher::new();
        fp.text(&goroot)
            .text(&go_version)
            .text(&deps.fingerprint)
            .text(gopls.map(|g| g.version.as_str()).unwrap_or(""))
            .text(if cgo_on { "cgo" } else { "no-cgo" });
        prepared.fingerprint = fp.finish().hex_prefix(32);
        prepared.status.push(format!("toolchain Go {go_version} ({goroot})"));
        if let Some(g) = gopls {
            prepared
                .status
                .push(format!("gopls {} for Go {go_version}", g.version));
        }
        if cgo_on {
            prepared
                .status
                .push(format!("cgo enabled for {} file(s) (build approval given)", cgo.len()));
        }
        prepared.status.extend(deps.notes.iter().cloned());
        prepared.toolchain = Some(toolchain);
        Ok(prepared)
    }

    fn outside_build(&self, path: &str, error: &str) -> Option<String> {
        if !path.ends_with(".go") {
            return None;
        }
        let lower = error.to_ascii_lowercase();
        [
            "no package metadata",
            "no packages found",
            "build constraints exclude",
            "excluded by build constraints",
            "not included in your workspace",
        ]
        .iter()
        .any(|p| lower.contains(p))
        .then(|| "excluded by build constraints (build tags or GOOS/GOARCH) on this machine".to_string())
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::Label
    }
}

#[cfg(test)]
#[path = "../../tests/unit/languages/go.rs"]
mod tests;
