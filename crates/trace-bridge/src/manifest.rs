//! Hand-written bridge manifests (codepath_next format, NEXT.md item 27), labelled
//! `user_contract`, tier inferred.
//!
//! ```json
//! {"schema": 1, "bridges": [{
//!   "kind": "contract_link" | "ffi_binding" | "generated_binding",
//!   "from": {"file": "web/api.ts", "sha256": "<hex>", "start_byte": 10, "end_byte": 90},
//!   "to":   {"file": "srv/app.py", "sha256": "<hex>", "start_byte": 0,  "end_byte": 50},
//!   "evidence": [{"file": "...", "sha256": "<hex>", "start_byte": 1, "end_byte": 2}],
//!   "assumptions": ["..."],
//!   "label": "optional display label"
//! }]}
//! ```
//!
//! Endpoints must match exactly one symbol span (`Symbol::span.bytes`) of the pinned file;
//! a file whose sha256 differs from the manifest is stale: the entry is dropped with a
//! `bridge_manifest_stale` diagnostic (never silently re-anchored).

use std::path::PathBuf;

use serde_json::Value;
use sha2::{Digest, Sha256};
use trace_core::model::{
    Bridge, BridgeKind, ByteSpan, FileId, Location, Provider, Resolution, SymbolId, Tier,
};

use crate::ctx::Ctx;

const KINDS: [&str; 3] = ["contract_link", "ffi_binding", "generated_binding"];
const MAX_MANIFEST_BYTES: u64 = 1_000_000;
const MAX_ENTRIES: usize = 1_000;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

enum EndpointError {
    Stale(String),
    Invalid(String),
}

struct Pinned {
    file: FileId,
    span: ByteSpan,
    line: u32,
}

fn pinned(ctx: &Ctx<'_>, v: &Value) -> Result<Pinned, EndpointError> {
    let invalid = |m: &str| EndpointError::Invalid(m.to_string());
    let path = v
        .get("file")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("endpoint without file"))?;
    let expected = v
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("endpoint without sha256"))?;
    let start = v
        .get("start_byte")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("endpoint without start_byte"))?;
    let end = v
        .get("end_byte")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("endpoint without end_byte"))?;
    let file = ctx
        .index
        .file_by_path(path)
        .ok_or_else(|| EndpointError::Stale(format!("{path} is not in the index")))?;
    let source = ctx
        .sources
        .file(file)
        .map_err(|e| EndpointError::Stale(format!("{path}: {e}")))?;
    if !sha256_hex(&source.bytes).eq_ignore_ascii_case(expected.trim()) {
        return Err(EndpointError::Stale(format!(
            "{path} changed since the manifest was written (sha256 mismatch)"
        )));
    }
    if !(start < end && end as usize <= source.bytes.len()) {
        return Err(invalid(&format!("{path}: span {start}..{end} is out of range")));
    }
    let span = ByteSpan::new(start as u32, end as u32);
    Ok(Pinned {
        file,
        span,
        line: source.lines.line1(span.start),
    })
}

fn endpoint_symbol(ctx: &Ctx<'_>, p: &Pinned) -> Option<SymbolId> {
    let hits: Vec<SymbolId> = ctx
        .index
        .symbols_of(p.file)
        .iter()
        .filter(|s| s.span.bytes == p.span)
        .map(|s| s.id)
        .collect();
    (hits.len() == 1).then(|| hits[0])
}

pub(crate) fn detect(ctx: &mut Ctx<'_>, manifests: &[PathBuf]) {
    for path in manifests {
        let shown = path.display().to_string();
        let data = match std::fs::metadata(path) {
            Ok(m) if m.len() > MAX_MANIFEST_BYTES => {
                ctx.diag("bridge_manifest_invalid", Some(shown), "manifest larger than 1 MB".into());
                continue;
            }
            Ok(_) => std::fs::read(path),
            Err(e) => Err(e),
        };
        let value: Option<Value> = data.ok().and_then(|b| serde_json::from_slice(&b).ok());
        let Some(value) = value else {
            ctx.diag("bridge_manifest_invalid", Some(shown), "manifest could not be read as JSON".into());
            continue;
        };
        let entries = value.get("bridges").and_then(Value::as_array);
        let (true, Some(entries)) = (value.get("schema").and_then(Value::as_u64) == Some(1), entries) else {
            ctx.diag(
                "bridge_manifest_invalid",
                Some(shown),
                "manifest needs {\"schema\": 1, \"bridges\": [...]}".into(),
            );
            continue;
        };
        if entries.len() > MAX_ENTRIES {
            ctx.diag("bridge_manifest_invalid", Some(shown), format!("more than {MAX_ENTRIES} entries"));
            continue;
        }
        for (n, b) in entries.iter().enumerate() {
            entry(ctx, &shown, n, b);
        }
    }
}

fn entry(ctx: &mut Ctx<'_>, shown: &str, n: usize, b: &Value) {
    let where_ = format!("{shown} entry {n}");
    let kind = b.get("kind").and_then(Value::as_str).unwrap_or("");
    let assumptions: Vec<String> = b
        .get("assumptions")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let evidence = b
        .get("evidence")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !KINDS.contains(&kind) || assumptions.is_empty() || evidence.is_empty() {
        ctx.diag(
            "bridge_manifest_invalid",
            Some(where_),
            "entries need a permitted kind (contract_link|ffi_binding|generated_binding), source evidence and assumptions".into(),
        );
        return;
    }
    let mut ends = Vec::new();
    for side in ["from", "to"] {
        let Some(v) = b.get(side) else {
            ctx.diag("bridge_manifest_invalid", Some(where_.clone()), format!("missing `{side}` endpoint"));
            return;
        };
        match pinned(ctx, v) {
            Ok(p) => match endpoint_symbol(ctx, &p) {
                Some(sym) => ends.push((sym, p)),
                None => {
                    ctx.diag(
                        "bridge_manifest_invalid",
                        Some(where_.clone()),
                        format!("`{side}` span matches no single symbol of {}", ctx.path(p.file)),
                    );
                    return;
                }
            },
            Err(EndpointError::Stale(m)) => {
                ctx.diag("bridge_manifest_stale", Some(where_.clone()), m);
                return;
            }
            Err(EndpointError::Invalid(m)) => {
                ctx.diag("bridge_manifest_invalid", Some(where_.clone()), m);
                return;
            }
        }
    }
    for e in &evidence {
        match pinned(ctx, e) {
            Ok(_) => {}
            Err(EndpointError::Stale(m)) => {
                ctx.diag("bridge_manifest_stale", Some(where_.clone()), format!("evidence: {m}"));
                return;
            }
            Err(EndpointError::Invalid(m)) => {
                ctx.diag("bridge_manifest_invalid", Some(where_.clone()), format!("evidence: {m}"));
                return;
            }
        }
    }
    let (to_sym, to) = ends.pop().expect("two endpoints");
    let (from_sym, from) = ends.pop().expect("two endpoints");
    let label = b
        .get("label")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| kind.to_string());
    let mut assumptions = assumptions;
    assumptions.push(format!("hand-written manifest ({kind}); not a runtime observation"));
    ctx.push(Bridge {
        kind: BridgeKind::UserContract,
        tier: Tier::Inferred,
        from: from_sym,
        to: to_sym,
        from_at: Location {
            file: from.file,
            bytes: from.span,
            line: from.line,
        },
        to_at: Location {
            file: to.file,
            bytes: to.span,
            line: to.line,
        },
        provider: Provider::UserContract,
        resolution: Resolution::Manifest,
        label,
        assumptions,
        candidates: 1,
        contract: None,
    });
}

#[cfg(test)]
#[path = "../tests/unit/manifest.rs"]
mod tests;
