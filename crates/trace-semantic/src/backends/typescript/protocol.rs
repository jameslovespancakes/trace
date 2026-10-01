//! The worker's JSON input and output shapes (`assets/ts-worker/main.mjs`).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize)]
pub(super) struct WorkerInput<'a> {
    pub(super) workspace: String,
    pub(super) repo_root: String,
    pub(super) files: Vec<WorkerFile<'a>>,
    /// Project configuration texts.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) configs: Vec<WorkerConfig>,
    /// Read-only `node_modules` mappings (virtual workspace path -> real directory).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) modules: Vec<WorkerModules>,
    /// trace's bundled Node types (only when the project has none).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) bundled_types: Vec<WorkerModules>,
    pub(super) names: Vec<&'a str>,
    /// Files to visit (all when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) query: Option<Vec<&'a str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) references: Option<WorkerQuery>,
}

#[derive(Serialize)]
pub(super) struct WorkerConfig {
    pub(super) path: String,
    pub(super) text: String,
}

#[derive(Serialize)]
pub(super) struct WorkerModules {
    #[serde(rename = "virtual")]
    pub(super) virtual_dir: String,
    #[serde(rename = "real")]
    pub(super) real_dir: String,
}

#[derive(Serialize)]
pub(super) struct WorkerFile<'a> {
    pub(super) path: &'a str,
    pub(super) source: &'a str,
}

/// One changed file of a serve-mode `update`.
#[derive(Serialize)]
pub(super) struct WorkerChange<'a> {
    pub(super) path: &'a str,
    pub(super) text: &'a str,
}

#[derive(Serialize)]
pub(super) struct WorkerQuery {
    pub(super) file: String,
    pub(super) start_byte: u32,
}

/// Worker output (unknown fields ignored).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct WorkerOutput {
    #[serde(default)]
    pub(super) symbols: HashMap<String, WorkerSymbol>,
    #[serde(default)]
    pub(super) edges: Vec<WorkerEdge>,
    #[serde(default)]
    pub(super) unresolved: Vec<WorkerUnresolved>,
    #[serde(default)]
    pub(super) uses: Vec<WorkerUse>,
    #[serde(default)]
    pub(super) references: Vec<WorkerReference>,
    #[serde(default)]
    pub(super) references_complete: Option<bool>,
    /// Function-type rule answers (`fntype.mjs`).
    #[serde(default)]
    pub(super) callback_params: Vec<WorkerCallbackParam>,
    /// Library files calls resolved into (`external.mjs`).
    #[serde(default)]
    pub(super) library_files: Vec<trace_core::semantics::LibraryFile>,
    #[serde(default)]
    pub(super) library_calls: Vec<WorkerLibraryCall>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkerSpan {
    pub(super) start: u32,
    pub(super) end: u32,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkerCallbackParam {
    pub(super) source: String,
    pub(super) call: WorkerSpan,
    pub(super) arg: WorkerSpan,
    #[serde(default)]
    pub(super) param_name: Option<String>,
    #[serde(default)]
    pub(super) param_type: String,
    pub(super) verdict: trace_core::semantics::FnTypeVerdict,
    #[serde(default)]
    pub(super) route: String,
    #[serde(default)]
    pub(super) library_symbol: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkerLibraryCall {
    pub(super) source: String,
    pub(super) at: WorkerSpan,
    #[serde(default)]
    pub(super) line: u32,
    pub(super) file: u32,
    #[serde(default)]
    pub(super) decl_line: u32,
    #[serde(default)]
    pub(super) decl_column: u32,
    #[serde(default)]
    pub(super) symbol: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkerSymbol {
    pub(super) file: String,
    pub(super) name: String,
    pub(super) start_byte: u32,
    pub(super) end_byte: u32,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkerEdge {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) kind: String,
    pub(super) evidence: WorkerEvidence,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkerUnresolved {
    pub(super) owner: Option<String>,
    #[serde(default)]
    pub(super) kind: String,
    pub(super) evidence: WorkerEvidence,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkerUse {
    #[serde(default)]
    pub(super) from: Option<String>,
    pub(super) to: String,
    pub(super) kind: String,
    pub(super) evidence: WorkerEvidence,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkerReference {
    pub(super) file: String,
    pub(super) start_byte: u32,
    pub(super) end_byte: u32,
    #[serde(default)]
    pub(super) is_declaration: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkerEvidence {
    pub(super) file: String,
    pub(super) start_byte: u32,
    pub(super) end_byte: u32,
    pub(super) line: u32,
}
