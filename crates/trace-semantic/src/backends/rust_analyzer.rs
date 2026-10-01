//! rust-analyzer protocol specifics (owner native).
//!
//! rust-analyzer is an ordinary registry entry (`assets/backends/rust.json`, `lsp:rust-analyzer`,
//! launched by `backends/generic.rs`); its setup lives in `languages/rust.rs`. This module holds what is
//! specific to rust-analyzer's protocol, shared by the hooks and their tests:
//!
//! * **Load health** ([`last_status`], [`classify_status`]): readiness is
//!   `experimental/serverStatus` with `quiescent: true`; its `health` / `message` say whether
//!   the Cargo workspace really loaded. "Failed to run build scripts" = the build failed;
//!   "Failed to read Cargo metadata" = dependencies missing (for the sysroot: the standard
//!   library's own crates are not downloaded); proc-macro server failures = procedural macros
//!   could not run; any other `error` health = the workspace did not load. Never ignored
//!   (PLAN decision 3).
//! * **Macro expansion** (DESIGN §1.15, bridges): under the build approval, attribute
//!   procedural macros on `fn` / `impl` / `mod` / `trait` items ([`expansion_targets`], a
//!   syntax rule: an outer attribute whose path is not a built-in attribute of the Rust
//!   reference or a tool attribute) are expanded with `rust-analyzer/expandMacro`
//!   ([`expand_params`], [`parse_expansion`]) and delivered in `FileSemantics::expanded`,
//!   bounded by [`MAX_EXPANSIONS_PER_RUN`].
//!
//! The former dedicated backend (generated `rust-project.json`, no build scripts, no std
//! resolution) is deleted: Cargo workspace loading replaces it (research: 32.8% -> 91.8% of
//! definition sites answered on the same repository).

use serde_json::{json, Value};
use trace_core::model::ByteSpan;
use trace_core::Language;

/// Upper bound of `rust-analyzer/expandMacro` requests per analysis run (a `bounded`
/// diagnostic reports the rest).
pub const MAX_EXPANSIONS_PER_RUN: u32 = 2_000;

/// The LSP method of macro expansion.
pub const EXPAND_MACRO: &str = "rust-analyzer/expandMacro";

/// The readiness / health notification.
pub const SERVER_STATUS: &str = "experimental/serverStatus";

/// The last `experimental/serverStatus` rust-analyzer sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerStatus {
    /// "ok" | "warning" | "error"
    pub health: String,
    pub quiescent: bool,
    pub message: Option<String>,
}

/// The last quiescent server status of `notifications` (else the last one at all).
pub fn last_status(notifications: &[(String, Value)]) -> Option<ServerStatus> {
    let all: Vec<ServerStatus> = notifications
        .iter()
        .filter(|(method, _)| method == SERVER_STATUS)
        .map(|(_, params)| ServerStatus {
            health: params
                .get("health")
                .and_then(Value::as_str)
                .unwrap_or("ok")
                .to_string(),
            quiescent: params.get("quiescent").and_then(Value::as_bool).unwrap_or(false),
            message: params
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|m| !m.trim().is_empty()),
        })
        .collect();
    all.iter().rev().find(|s| s.quiescent).or_else(|| all.last()).cloned()
}

/// Why the Cargo workspace did not load completely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadProblem {
    /// Build scripts of some packages failed.
    BuildScripts,
    /// The proc-macro server could not load or run procedural macros.
    ProcMacros,
    /// `cargo metadata` could not resolve the project's dependencies offline.
    Dependencies,
    /// The standard library's own dependencies are not in the Cargo home.
    SysrootDependencies,
    /// The workspace did not load (message kept).
    Workspace(String),
}

/// Map a server status to a load problem (tool output, not source code). `ok` health and
/// warnings without a known cause are not problems.
pub fn classify_status(status: &ServerStatus) -> Option<LoadProblem> {
    if status.health == "ok" {
        return None;
    }
    let message = status.message.clone().unwrap_or_default();
    let lower = message.to_ascii_lowercase();
    let offline = [
        "attempting to make an http request, but --offline",
        "failed to select a version",
        "failed to download",
        "no matching package",
        "failed to load source for dependency",
        "--offline",
    ]
    .iter()
    .any(|p| lower.contains(p));
    if lower.contains("failed to run build scripts") {
        return Some(LoadProblem::BuildScripts);
    }
    if lower.contains("cargo metadata") && lower.contains("sysroot") {
        return Some(LoadProblem::SysrootDependencies);
    }
    if lower.contains("failed to read cargo metadata") || (lower.contains("cargo metadata") && offline) {
        return Some(if offline || lower.contains("with dependencies") {
            LoadProblem::Dependencies
        } else {
            LoadProblem::Workspace(message)
        });
    }
    if lower.contains("proc-macro") || lower.contains("proc macro") {
        return Some(LoadProblem::ProcMacros);
    }
    if status.health == "error" {
        return Some(LoadProblem::Workspace(message));
    }
    None
}

/// One attribute-macro invocation to expand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroTarget {
    /// The whole attribute (`#[...]`) in the file.
    pub span: ByteSpan,
    /// LSP position of the attribute path (0-based line, UTF-16 column).
    pub line: u32,
    pub character: u32,
    /// The attribute path as written (`tokio::main`).
    pub path: String,
}

/// Built-in attributes of the Rust reference (never procedural macros).
const BUILTIN_ATTRIBUTES: &[&str] = &[
    "cfg",
    "cfg_attr",
    "test",
    "ignore",
    "should_panic",
    "derive",
    "automatically_derived",
    "macro_export",
    "macro_use",
    "proc_macro",
    "proc_macro_derive",
    "proc_macro_attribute",
    "allow",
    "warn",
    "deny",
    "forbid",
    "expect",
    "deprecated",
    "must_use",
    "link",
    "link_name",
    "link_ordinal",
    "no_link",
    "repr",
    "crate_type",
    "no_main",
    "export_name",
    "link_section",
    "no_mangle",
    "used",
    "crate_name",
    "inline",
    "cold",
    "no_builtins",
    "target_feature",
    "track_caller",
    "instruction_set",
    "doc",
    "no_std",
    "no_implicit_prelude",
    "path",
    "recursion_limit",
    "type_length_limit",
    "panic_handler",
    "global_allocator",
    "windows_subsystem",
    "feature",
    "non_exhaustive",
    "debugger_visualizer",
    "collapse_debuginfo",
    "unsafe",
    "naked",
    "bench",
    "rustc_builtin_macro",
    "coverage",
    "optimize",
    "unstable",
    "stable",
    "allow_internal_unstable",
];

/// Tool attribute namespaces (lint tools and the like; never procedural macros).
const TOOL_NAMESPACES: &[&str] = &["rustfmt", "clippy", "rust_analyzer", "diagnostic", "rustdoc"];

/// Items whose attribute macros are expanded.
const EXPANDED_ITEMS: &[&str] = &["function_item", "impl_item", "mod_item", "trait_item"];

/// Whether an attribute path names a built-in or tool attribute.
pub fn is_builtin_attribute(path: &str) -> bool {
    let path = path.trim();
    match path.split_once("::") {
        Some((first, _)) => TOOL_NAMESPACES.contains(&first.trim()),
        None => BUILTIN_ATTRIBUTES.contains(&path),
    }
}

/// Attribute-macro invocations on `fn` / `impl` / `mod` / `trait` items of a Rust source
/// (module docs), in file order.
pub fn expansion_targets(source: &[u8]) -> Vec<MacroTarget> {
    let mut out = Vec::new();
    let Ok(tree) = trace_syntax::parse_tree(Language::Rust, source) else {
        return out;
    };
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.kind() == "attribute_item" {
            let path_node = node
                .named_child(0)
                .filter(|a| a.kind() == "attribute")
                .and_then(|a| a.named_child(0));
            let mut item = node.next_named_sibling();
            while let Some(s) = item {
                if matches!(s.kind(), "attribute_item" | "line_comment" | "block_comment") {
                    item = s.next_named_sibling();
                } else {
                    break;
                }
            }
            if let (Some(path_node), Some(item)) = (path_node, item) {
                let path = path_node.utf8_text(source).unwrap_or_default().to_string();
                if EXPANDED_ITEMS.contains(&item.kind()) && !path.is_empty() && !is_builtin_attribute(&path) {
                    let (line, character) = lsp_position(source, path_node.start_byte());
                    out.push(MacroTarget {
                        span: ByteSpan {
                            start: node.start_byte() as u32,
                            end: node.end_byte() as u32,
                        },
                        line,
                        character,
                        path,
                    });
                }
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return out;
            }
        }
    }
}

/// 0-based line and UTF-16 column of byte `offset`.
fn lsp_position(source: &[u8], offset: usize) -> (u32, u32) {
    let offset = offset.min(source.len());
    let before = &source[..offset];
    let line_start = before.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let line = before.iter().filter(|b| **b == b'\n').count();
    let column: usize = String::from_utf8_lossy(&before[line_start..])
        .chars()
        .map(char::len_utf16)
        .sum();
    (line as u32, column as u32)
}

/// `rust-analyzer/expandMacro` parameters for a target of the document `uri`.
pub fn expand_params(uri: &str, target: &MacroTarget) -> Value {
    json!({
        "textDocument": {"uri": uri},
        "position": {"line": target.line, "character": target.character}
    })
}

/// The expansion text of an `expandMacro` answer (`{"name", "expansion"}`); `None` for null or
/// an empty expansion.
pub fn parse_expansion(answer: &Value) -> Option<String> {
    answer
        .get("expansion")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|e| !e.trim().is_empty())
}

#[cfg(test)]
#[path = "../../tests/unit/backends/rust_analyzer.rs"]
mod tests;
