//! Weak boundaries (possible only): subprocess / script invocation of a repository file and
//! message or event names published in one language and consumed in another.
//!
//! A process call's command line comes from a derived `Sends { channel: Process }` effect or
//! an irreducible spawn primitive (`recognize`); each token names a repository script when it
//! is the path of an indexed file whose language trace's inventory knows (extension or
//! shebang) - no package knowledge.

use std::collections::HashMap;

use trace_core::facts::BoundaryRole;
use trace_core::model::{BridgeKind, FileId, Provider, Resolution, Tier};

use crate::ctx::{join_rel, parent_dir, Ctx, End, Fact};

/// Repository file a script token names: exact repository path, relative to the caller's
/// directory, or a unique path suffix.
fn script_file(ctx: &Ctx<'_>, caller: FileId, token: &str) -> Option<FileId> {
    let token = token.trim_start_matches("./");
    if let Some(f) = ctx.index.file_by_path(token) {
        return Some(f);
    }
    if let Some(p) = join_rel(parent_dir(ctx.path(caller)), token) {
        if let Some(f) = ctx.index.file_by_path(&p) {
            return Some(f);
        }
    }
    if token.contains("..") {
        return None;
    }
    let suffix = format!("/{token}");
    let hits: Vec<FileId> = ctx
        .index
        .files
        .iter()
        .enumerate()
        .filter(|(_, r)| r.path.ends_with(&suffix))
        .map(|(i, _)| FileId(i as u32))
        .collect();
    (hits.len() == 1).then(|| hits[0])
}

pub(crate) fn subprocess(ctx: &mut Ctx<'_>) {
    for u in ctx.of(BridgeKind::Subprocess, BoundaryRole::Uses) {
        let Some(file) = script_file(ctx, u.file, u.name()) else { continue };
        if file == u.file || !ctx.index.files[file.idx()].language.is_code() {
            continue;
        }
        let Some(target) = ctx
            .module_symbol(file)
            .or_else(|| ctx.index.symbols_of(file).first().map(|s| s.id))
        else {
            continue;
        };
        let Some(from) = ctx.end_of(&u) else { continue };
        let to = ctx.symbol_end(target);
        let call = u.detail("call").unwrap_or("process call");
        ctx.emit(
            BridgeKind::Subprocess,
            from,
            &[to],
            Tier::Possible,
            Provider::Rule("subprocess".into()),
            Resolution::GeneratedCandidate,
            &format!("run {}", ctx.path(file)),
            &[format!("`{call}` starts a process whose arguments name this repository file; the interpreter and working directory are assumed")],
            None,
        );
    }
}

pub(crate) fn messages(ctx: &mut Ctx<'_>) {
    let subscribers = ctx.of(BridgeKind::Message, BoundaryRole::Provides);
    if subscribers.is_empty() {
        return;
    }
    let mut by_topic: HashMap<&str, Vec<Fact<'_>>> = HashMap::new();
    for s in &subscribers {
        by_topic.entry(s.name()).or_default().push(*s);
    }
    for p in ctx.of(BridgeKind::Message, BoundaryRole::Uses) {
        let Some(subs) = by_topic.get(p.name()) else { continue };
        let others: Vec<Fact<'_>> = subs.iter().filter(|s| s.language != p.language).copied().collect();
        if others.is_empty() {
            continue;
        }
        let Some(from) = ctx.end_of(&p) else { continue };
        let cands: Vec<End> = others.iter().filter_map(|s| ctx.end_of(s)).collect();
        ctx.emit(
            BridgeKind::Message,
            from,
            &cands,
            Tier::Possible,
            Provider::Rule("message".into()),
            Resolution::GeneratedCandidate,
            &format!("topic:{}", p.name()),
            &["the same literal topic / event name is published here and subscribed in another language; the broker and channel are assumed shared".into()],
            None,
        );
    }
}
