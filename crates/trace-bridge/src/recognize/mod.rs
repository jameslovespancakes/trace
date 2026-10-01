//! Per-file endpoints (PLAN decision 14, DESIGN §1.15, DESIGN-bridges §2-§5).
//!
//! trace knows no framework or package by name. The endpoints of one file are:
//!
//! 1. **Channel effects of its library calls** (`LibraryKnowledge::by_call`, derived from the
//!    installed package source by trace-library): `Sends` (client calls), `Registers` (handler
//!    registrations), `Mounts` (sub-registries under a prefix; a `Mounts` whose target is the
//!    constructed object itself is the object's own prefix), `Decorates` (the decorated
//!    declaration receives the inner effect), `Exports` (callables exposed to another
//!    language, incl. the exports of expanded macro code). Keys, verbs and handlers are the
//!    call's argument values, evaluated lazily from the syntax tree
//!    (`trace_syntax::boundary::eval_argument`).
//! 2. **Direct calls of irreducible primitives**: a library call whose server-given symbol is
//!    an `io_send` / `io_entry` / `ffi_conventions` row (runtime built-ins without source).
//! 3. **`fs_routes` rows** whose activating package is installed and whose glob matches the
//!    file (filesystem routing conventions).
//! 4. **Reflection roots**: annotations whose meta-annotation chain (read from the annotation
//!    type declarations that are available) reaches a `reflection_roots` row; attribute rows
//!    of `ffi_conventions` (platform-invoke declarations).
//! 5. **Language / ABI / protocol facts of the syntax tree** (`FileFacts::boundaries`): C
//!    symbols, JNI names, cgo, CPython C-API method tables, Node-API, GraphQL documents,
//!    Python constants used by mount prefixes, and the facts of the irreducible
//!    `syntax_conventions` rows (Rust binding attributes, names of generated RPC code,
//!    GraphQL resolver conventions, addon loaders; each row says why it cannot be derived).
//!    Every syntax fact is an endpoint as it is (DESIGN-bridges §6).
//!
//! Endpoints are `BoundaryFact`s (the shape the matchers read). Derived endpoints carry
//! `derived=true`: a crossing built on one is `inferred` only when the language's bridge gate
//! passed, else `possible` (lib.rs). Endpoints of a file depend only on that file (its
//! source, facts, semantics and knowledge entries) and on global inputs (tables, installed
//! packages, the gate, the annotation declarations of the repository): cached per file by
//! the incremental detection.

use std::collections::{BTreeMap, BTreeSet};

use trace_core::facts::{BoundaryFact, BoundaryRole, CallSite, FileFacts};
use trace_core::model::{BridgeKind, ByteSpan, FileId, FileRecord};
use trace_core::semantics::FileSemantics;
use trace_core::Language;
use trace_library::channels::{annotation_chain, rows_of, ChannelRow};
use trace_library::model::{ArgSel, CallBehaviour, Channel, Effect, VerbSel};
use trace_library::table::{RowSel, RowVerb, Section};
use trace_syntax::boundary::{ArgRef, ArgValue, ParsedFile, Tpl, TplPart};

use crate::http::{join_paths, normalize, verb_of, Placeholders};
use crate::BridgeInput;

mod chain;
mod fs_routes;
mod reflect;
mod url_literals;

use url_literals::{command_tokens, literal_prefix, looks_like_url, names_a_file};

/// Detail key marking an endpoint built from derived channel effects.
pub(crate) const DERIVED: &str = "derived";

/// One endpoint of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub file: FileId,
    pub language: Language,
    pub fact: BoundaryFact,
}

/// The endpoints of one file and whether every part could be read.
#[derive(Debug, Default)]
pub(crate) struct FileOut {
    pub facts: Vec<BoundaryFact>,
    /// The file's source could not be read (derived endpoints missing; not cached).
    pub unreadable: bool,
}

/// Shared, file-independent inputs of the recognition.
pub(crate) struct Recognizer<'i, 'a> {
    input: &'i BridgeInput<'a>,
    pub(crate) placeholders: Placeholders,
    /// Annotation type declarations of the repository per language (reflection chains).
    annotation_sources: BTreeMap<Language, Vec<Vec<u8>>>,
}

impl<'i, 'a> Recognizer<'i, 'a> {
    /// `annotation_files`: repository files that declare annotation types (see
    /// [`annotation_files`]).
    pub(crate) fn new(
        input: &'i BridgeInput<'a>,
        annotation_files: &BTreeMap<String, AnnotationFile>,
    ) -> Self {
        let mut annotation_sources: BTreeMap<Language, Vec<Vec<u8>>> = BTreeMap::new();
        for (path, f) in annotation_files {
            if !f.declares {
                continue;
            }
            let Some(id) = input.index.file_by_path(path) else { continue };
            let language = input.index.files[id.idx()].language;
            if let Ok(src) = input.sources.file(id) {
                annotation_sources
                    .entry(language)
                    .or_default()
                    .push(src.bytes.clone());
            }
        }
        Recognizer {
            input,
            placeholders: Placeholders::from_tables(input.tables),
            annotation_sources,
        }
    }

    fn installed(&self, language: Language, package: &str) -> bool {
        match trace_env::EcosystemId::of_language(language) {
            Some(eco) => self.input.installed.contains(eco, package),
            None => false,
        }
    }

    /// Every endpoint of `file`.
    pub(crate) fn file(&self, file: FileId) -> FileOut {
        let mut out = FileOut::default();
        let Some(rec) = self.input.index.files.get(file.idx()) else {
            return out;
        };
        let Some(facts) = rec.facts.as_ref() else {
            return out;
        };
        out.facts.extend(facts.boundaries.iter().cloned());
        let language = rec.language;
        let calls = self.channel_calls(rec);
        let fs_rows = self.fs_route_rows(rec);
        let reflective = self.reflective(rec, facts);
        let strings = facts
            .call_details
            .iter()
            .any(|d| d.arguments.iter().any(|a| a.has_string));
        if calls.is_empty() && fs_rows.is_empty() && !reflective && !strings {
            finish(&mut out.facts);
            return out;
        }
        let source = match self.input.sources.file(file) {
            Ok(s) => s,
            Err(_) => {
                out.unreadable = true;
                finish(&mut out.facts);
                return out;
            }
        };
        let Some(parsed) = ParsedFile::parse(language, &source.bytes) else {
            finish(&mut out.facts);
            return out;
        };
        let site = Site {
            rec,
            facts,
            semantics: rec.semantic.as_ref(),
            parsed: &parsed,
            lines: &source.lines,
        };
        let prefixes = self.own_prefixes(&site, &calls);
        for (start, via, effects, derived) in &calls {
            for effect in effects {
                let call = self.call_for(&site, *start, effect);
                self.apply(&site, *start, call, effect, None, via, *derived, &prefixes, &mut out.facts);
            }
        }
        if strings {
            self.url_literals(&site, &calls, &mut out.facts);
        }
        for row in &fs_rows {
            self.fs_route(&site, row, &mut out.facts);
        }
        if reflective {
            self.reflection(&site, &mut out.facts);
        }
        finish(&mut out.facts);
        out
    }

    /// Library calls of the file with channel effects: derived / table behaviours from the
    /// knowledge, and direct calls of irreducible primitives. `(callee start, via, effects,
    /// derived)`, sorted by callee start.
    #[allow(clippy::type_complexity)]
    fn channel_calls(&self, rec: &FileRecord) -> Vec<(u32, String, Vec<Effect>, bool)> {
        let mut by_start: BTreeMap<u32, (String, Vec<Effect>, bool)> = BTreeMap::new();
        let path = rec.path.clone();
        for ((_, start), b) in self
            .input
            .knowledge
            .by_call
            .range((path.clone(), 0)..=(path, u32::MAX))
        {
            let effects: Vec<Effect> = b.effects.iter().filter(|e| e.is_channel()).cloned().collect();
            if effects.is_empty() {
                continue;
            }
            by_start
                .insert(*start, (via_of(b), effects, b.source == trace_library::BehaviourSource::Derived));
        }
        if let Some(sem) = rec.semantic.as_ref() {
            for (start, symbol, effects) in self.primitive_calls(rec.language, sem) {
                let entry = by_start
                    .entry(start)
                    .or_insert_with(|| (symbol.clone(), Vec::new(), false));
                for e in effects {
                    if !entry.1.contains(&e) {
                        entry.1.push(e);
                    }
                }
            }
        }
        by_start.into_iter().map(|(s, (v, e, d))| (s, v, e, d)).collect()
    }

    /// Calls whose server-given symbol is an irreducible primitive row.
    fn primitive_calls(&self, language: Language, sem: &FileSemantics) -> Vec<(u32, String, Vec<Effect>)> {
        let mut out = Vec::new();
        for lc in &sem.library_calls {
            let Some(symbol) = lc.symbol.as_deref() else { continue };
            let mut languages = vec![language];
            if let Some(f) = sem.library_files.get(lc.file as usize) {
                if !languages.contains(&f.language) {
                    languages.push(f.language);
                }
            }
            let mut effects = Vec::new();
            for l in languages {
                for row in self.active_rows(l, Section::IoSend) {
                    if row.symbol.as_deref() == Some(symbol) {
                        if let Some(key) = row.key.clone() {
                            effects.push(Effect::Sends {
                                channel: row.channel_or_default(),
                                key,
                                verb: row.verb.clone().unwrap_or(VerbSel::Any),
                            });
                        }
                    }
                }
                for row in self.active_rows(l, Section::IoEntry) {
                    if row.symbol.as_deref() == Some(symbol) {
                        if let (Some(key), Some(handler)) = (row.key.clone(), row.handler.clone()) {
                            effects.push(Effect::Registers {
                                channel: row.channel_or_default(),
                                key,
                                handler,
                                verb: row.verb.clone().unwrap_or(VerbSel::Any),
                            });
                        }
                    }
                }
                for row in self.active_rows(l, Section::FfiConventions) {
                    if row.symbol.as_deref() == Some(symbol) {
                        if let Some(key) = row.key.clone() {
                            effects.push(Effect::Sends {
                                channel: Channel::Ffi,
                                key,
                                verb: VerbSel::Any,
                            });
                        }
                    }
                }
            }
            if !effects.is_empty() {
                out.push((lc.at.start, symbol.to_string(), effects));
            }
        }
        out
    }

    /// Typed rows of a section whose activating package (if any) is installed.
    fn active_rows(&self, language: Language, section: Section) -> Vec<ChannelRow> {
        rows_of(self.input.tables, language, section)
            .into_iter()
            .filter(|r| r.activated_by.as_deref().is_none_or(|p| self.installed(language, p)))
            .collect()
    }

    /// Own prefixes of objects constructed in this file: a constructor call whose `Mounts`
    /// effect targets the constructed object itself, bound to a name (`router =
    /// Router(prefix="/items")`). Name -> normalized prefix.
    fn own_prefixes(
        &self,
        site: &Site<'_>,
        calls: &[(u32, String, Vec<Effect>, bool)],
    ) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = BTreeMap::new();
        for (start, _, effects, _) in calls {
            for e in effects {
                let Effect::Mounts {
                    key,
                    target: ArgSel::Receiver,
                } = e
                else {
                    continue;
                };
                let Some(callee) = callee_span(site.facts, *start) else { continue };
                let Some(name) = site.parsed.bound_name(callee) else { continue };
                let Some(value) = self.arg(site, callee, key) else { continue };
                let Some(tpl) = value.template else { continue };
                if let Some(norm) = normalize(&tpl, false, &self.placeholders) {
                    if !norm.dynamic && !norm.dynamic_prefix {
                        // A group of a group (`admin := v1.Group("/admin")`): under the
                        // receiver's own prefix too.
                        let outer = site
                            .facts
                            .calls
                            .iter()
                            .find(|c| c.callee_span.start == *start)
                            .and_then(|c| c.receiver.as_ref())
                            .and_then(|r| out.get(r));
                        let path = match outer {
                            Some(p) => join_paths(p, &norm.path),
                            None => norm.path,
                        };
                        out.insert(name, path);
                    }
                }
            }
        }
        out
    }

    fn arg(&self, site: &Site<'_>, callee: ByteSpan, sel: &ArgSel) -> Option<ArgValue> {
        let r = arg_ref(sel)?;
        site.parsed.eval_argument(callee, &r, &|_| None)
    }

    /// Apply one channel effect at a call (`handler`: the decorated declaration / applied
    /// function for `Decorates`).
    #[allow(clippy::too_many_arguments)]
    fn apply(
        &self,
        site: &Site<'_>,
        start: u32,
        call: Option<&CallSite>,
        effect: &Effect,
        handler: Option<Handler>,
        via: &str,
        derived: bool,
        prefixes: &BTreeMap<String, String>,
        out: &mut Vec<BoundaryFact>,
    ) {
        match effect {
            Effect::Sends { channel, key, verb } => {
                let Some(call) = call else { return };
                self.send(site, call, *channel, key, verb, via, derived, out);
            }
            Effect::Registers {
                channel,
                key,
                handler: hsel,
                verb,
            } => {
                let Some(call) = call else { return };
                let h = match handler {
                    Some(h) => Some(h),
                    None => self
                        .arg(site, call.callee_span, hsel)
                        .map(|v| self.handler_of(site, &v)),
                };
                // A registration whose handler argument is not passed at this call registers
                // nothing here (an optional handler parameter of a constructor).
                let Some(h) = h else {
                    return;
                };
                // Rule 3 at the call: a registry of this file registered as the handler of
                // another registry is mounted under the key (`app.use("/p", router)` where
                // `router.get(..)` registers routes).
                if *channel == Channel::Http && self.is_registry(site, call, &h.text) {
                    self.mount(site, call, key, hsel, via, derived, out);
                    return;
                }
                self.register(site, call, *channel, key, verb, Some(h), via, derived, prefixes, out);
            }
            Effect::Mounts { key, target } => {
                let Some(call) = call else { return };
                if matches!(target, ArgSel::Receiver) {
                    // The constructed object's own prefix (`own_prefixes`).
                    return;
                }
                self.mount(site, call, key, target, via, derived, out);
            }
            Effect::Decorates { inner } => {
                let Some(call) = call else { return };
                let target = match site.parsed.applied_to(call.callee_span) {
                    Some((_, arg)) => {
                        let decl = decl_at(site.facts, arg);
                        Some(Handler {
                            decl,
                            span: arg,
                            text: site.parsed.text(arg).trim().to_string(),
                        })
                    }
                    None => decorated_declaration(site.facts, call.span).map(|d| {
                        let decl = &site.facts.declarations[d as usize];
                        Handler {
                            decl: Some(d),
                            span: decl.name_span,
                            text: decl.name.clone(),
                        }
                    }),
                };
                if target.is_none() {
                    return;
                }
                self.apply(site, start, Some(call), inner, target, via, derived, prefixes, out);
            }
            Effect::Exports { channel: _, name } => {
                self.export(site, start, call, name, via, derived, out);
            }
            _ => {}
        }
    }

    /// Whether `name` is a registry of this file: another call on the receiver `name` (not
    /// `call`'s own receiver) registers HTTP handlers.
    fn is_registry(&self, site: &Site<'_>, call: &CallSite, name: &str) -> bool {
        if name.is_empty()
            || call.receiver.as_deref() == Some(name)
            || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        {
            return false;
        }
        self.channel_calls(site.rec).iter().any(|(start, _, effects, _)| {
            *start != call.callee_span.start
                && effects.iter().any(|e| {
                    matches!(
                        e,
                        Effect::Registers {
                            channel: Channel::Http,
                            ..
                        }
                    )
                })
                && site
                    .facts
                    .calls
                    .iter()
                    .any(|c| c.callee_span.start == *start && c.receiver.as_deref() == Some(name))
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn send(
        &self,
        site: &Site<'_>,
        call: &CallSite,
        channel: Channel,
        key: &ArgSel,
        verb: &VerbSel,
        via: &str,
        derived: bool,
        out: &mut Vec<BoundaryFact>,
    ) {
        let common = common_detail(via, derived);
        // A call resolved to the member-lookup hook looks up the member it spells.
        if *key == ArgSel::Member {
            let Some(name) = call.member.clone().filter(|m| !m.is_empty()) else { return };
            let kind = match channel {
                Channel::Ffi => BridgeKind::Ffi,
                _ => return,
            };
            let mut detail = common;
            detail.push(("loader".into(), via.to_string()));
            out.push(fact(site, kind, BoundaryRole::Uses, name, call.owner, None, call.span, detail));
            return;
        }
        let Some(value) = self.arg(site, call.callee_span, key) else { return };
        match channel {
            Channel::Process => {
                let mut tokens = value.template.as_ref().map(command_tokens).unwrap_or_default();
                // The rest of the command line: every positional argument after the program.
                if let ArgSel::Pos(i) | ArgSel::Rest(i) | ArgSel::Command(i) = key {
                    for k in (i + 1)..(i + 8) {
                        match self.arg(site, call.callee_span, &ArgSel::Pos(k)) {
                            Some(v) => {
                                tokens.extend(v.template.as_ref().map(command_tokens).unwrap_or_default())
                            }
                            None => break,
                        }
                    }
                }
                let mut seen = BTreeSet::new();
                for token in tokens.into_iter().filter(|t| names_a_file(t)) {
                    if !seen.insert(token.clone()) {
                        continue;
                    }
                    let mut detail = common.clone();
                    detail.push(("call".into(), call.callee.clone()));
                    out.push(fact(
                        site,
                        BridgeKind::Subprocess,
                        BoundaryRole::Uses,
                        token,
                        call.owner,
                        None,
                        call.span,
                        detail,
                    ));
                }
            }
            Channel::Ffi => {
                let Some(name) = value.template.as_ref().and_then(Tpl::plain) else { return };
                if name.is_empty() {
                    return;
                }
                let mut detail = common;
                detail.push(("loader".into(), via.to_string()));
                out.push(fact(
                    site,
                    BridgeKind::Ffi,
                    BoundaryRole::Uses,
                    name,
                    call.owner,
                    None,
                    call.span,
                    detail,
                ));
            }
            Channel::Rpc => {
                let Some(key) = value.template.as_ref().and_then(Tpl::plain) else { return };
                let Some((package, service, method)) = rpc_key(&key) else { return };
                let mut detail = common;
                detail.push(("package".into(), package));
                out.push(fact(
                    site,
                    BridgeKind::Grpc,
                    BoundaryRole::Uses,
                    format!("{service}/{method}"),
                    call.owner,
                    None,
                    call.span,
                    detail,
                ));
            }
            Channel::Http | Channel::Message => {
                let Some(tpl) = value.template.as_ref() else { return };
                // Rule 10: a raw transport's key decides by its shape (URL / path -> HTTP).
                let http = channel == Channel::Http || looks_like_url(tpl);
                if !http {
                    let Some(topic) = tpl.plain().filter(|t| !t.is_empty()) else { return };
                    out.push(fact(
                        site,
                        BridgeKind::Message,
                        BoundaryRole::Uses,
                        topic,
                        call.owner,
                        None,
                        call.span,
                        common,
                    ));
                    return;
                }
                let Some(norm) = normalize(tpl, true, &self.placeholders) else { return };
                let verbs = match &norm.verb {
                    Some(v) => vec![v.clone()],
                    // HTTP clients send GET when no method is given (Fetch / XHR / RFC 9110 default).
                    None => self.verbs(site, call, verb, "GET"),
                };
                for m in verbs {
                    let mut detail = common.clone();
                    detail.push(("method".into(), m.clone()));
                    detail.push(("path".into(), norm.path.clone()));
                    if norm.dynamic {
                        detail.push(("dynamic".into(), "true".into()));
                    }
                    if norm.dynamic_prefix {
                        detail.push(("dynamic_prefix".into(), "true".into()));
                    }
                    out.push(fact(
                        site,
                        BridgeKind::Http,
                        BoundaryRole::Uses,
                        format!("{m} {}", norm.path),
                        call.owner,
                        None,
                        call.span,
                        detail,
                    ));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn register(
        &self,
        site: &Site<'_>,
        call: &CallSite,
        channel: Channel,
        key: &ArgSel,
        verb: &VerbSel,
        handler: Option<Handler>,
        via: &str,
        derived: bool,
        prefixes: &BTreeMap<String, String>,
        out: &mut Vec<BoundaryFact>,
    ) {
        let keyed = self.arg(site, call.callee_span, key).and_then(|v| v.template);
        // HTTP: the derived key is the registry's key (a route table's endpoint name, `**options`
        // collected keywords); when it is no URL path at this call, the route is the call's one
        // URL-path argument (protocol shape: an absolute path).
        let path_key = |t: &Tpl| normalize(t, true, &self.placeholders).is_some_and(|n| !n.dynamic_prefix);
        let tpl = match keyed {
            Some(t) if channel != Channel::Http || path_key(&t) => t,
            other => {
                let paths: Vec<Tpl> = if channel == Channel::Http {
                    site.parsed
                        .call_strings(call.callee_span, &|_| None)
                        .into_iter()
                        .filter_map(|s| s.value.template)
                        .filter(|t| path_key(t))
                        .collect()
                } else {
                    Vec::new()
                };
                match (paths.len(), other) {
                    (1, _) => paths.into_iter().next().expect("one path"),
                    (_, Some(t)) => t,
                    _ => return,
                }
            }
        };
        let tpl = &tpl;
        let mut detail = common_detail(via, derived);
        let decl = handler.as_ref().and_then(|h| h.decl);
        if let Some(h) = &handler {
            detail.push(("handler_span".into(), format!("{}:{}", h.span.start, h.span.end)));
            if !h.text.is_empty() && h.text.len() <= 200 {
                detail.push(("handler".into(), h.text.clone()));
            }
        }
        match channel {
            Channel::Http => {
                let Some(norm) = normalize(tpl, false, &self.placeholders) else { return };
                let own = call.receiver.as_ref().and_then(|r| prefixes.get(r));
                let path = match own {
                    Some(p) => {
                        detail.push(("own_prefix".into(), p.clone()));
                        join_paths(p, &norm.path)
                    }
                    None => norm.path.clone(),
                };
                detail.push(("framework".into(), format!("derived ({via})")));
                if let Some(r) = &call.receiver {
                    detail.push(("router".into(), r.clone()));
                    detail.push(("receiver_unknown".into(), "true".into()));
                }
                if norm.dynamic || norm.dynamic_prefix {
                    detail.push(("dynamic".into(), "true".into()));
                }
                let verbs = match &norm.verb {
                    Some(v) => vec![v.clone()],
                    None => self.verbs(site, call, verb, "*"),
                };
                for m in verbs {
                    let mut d = detail.clone();
                    d.push(("method".into(), m.clone()));
                    d.push(("path".into(), path.clone()));
                    out.push(fact(
                        site,
                        BridgeKind::Http,
                        BoundaryRole::Provides,
                        format!("{m} {path}"),
                        call.owner,
                        decl,
                        call.span,
                        d,
                    ));
                }
            }
            Channel::Message => {
                let Some(topic) = tpl.plain().filter(|t| !t.is_empty()) else { return };
                out.push(fact(
                    site,
                    BridgeKind::Message,
                    BoundaryRole::Provides,
                    topic,
                    call.owner,
                    decl,
                    call.span,
                    detail,
                ));
            }
            Channel::Rpc => {
                let Some(key) = tpl.plain() else { return };
                let Some((package, service, method)) = rpc_key(&key) else { return };
                detail.push(("package".into(), package));
                detail.push(("service".into(), service.clone()));
                out.push(fact(
                    site,
                    BridgeKind::Grpc,
                    BoundaryRole::Provides,
                    format!("{service}/{method}"),
                    call.owner,
                    decl,
                    call.span,
                    detail,
                ));
            }
            Channel::Process | Channel::Ffi => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn mount(
        &self,
        site: &Site<'_>,
        call: &CallSite,
        key: &ArgSel,
        target: &ArgSel,
        via: &str,
        derived: bool,
        out: &mut Vec<BoundaryFact>,
    ) {
        let Some(target) = self.arg(site, call.callee_span, target) else { return };
        let mounted = match (target.callable, target.template.as_ref().and_then(Tpl::plain)) {
            (_, Some(module))
                if site.rec.language == Language::Python && !module.is_empty() && !module.contains('/') =>
            {
                format!("module:{module}")
            }
            (Some(span), _) => site.parsed.text(span).trim().to_string(),
            _ => return,
        };
        if mounted.is_empty() {
            return;
        }
        let mut detail = common_detail(via, derived);
        detail.push(("mount".into(), "true".into()));
        detail.push(("mounted".into(), mounted));
        detail.push(("receiver_state".into(), "derived".into()));
        if let Some(r) = &call.receiver {
            detail.push(("receiver".into(), r.clone()));
        }
        let value = self.arg(site, call.callee_span, key);
        let norm = value
            .as_ref()
            .and_then(|v| v.template.as_ref())
            .and_then(|t| normalize(t, false, &self.placeholders));
        let prefix = match (&norm, &value) {
            (Some(n), _) if !n.dynamic && !n.dynamic_prefix => n.path.clone(),
            (_, Some(v)) => {
                // A non-literal prefix: its spelling resolves through constants at match time.
                detail.push(("dynamic".into(), "true".into()));
                if v.callable.is_some() {
                    detail.push(("prefix_ref".into(), site.parsed.text(v.span).trim().to_string()));
                }
                let base = v.template.as_ref().map(literal_prefix).unwrap_or_default();
                detail.push(("prefix_base".into(), if base.is_empty() { "/".into() } else { base.clone() }));
                if base.is_empty() {
                    "/".to_string()
                } else {
                    base
                }
            }
            // No prefix argument: mounted at the receiver's root.
            _ => "/".to_string(),
        };
        detail.push(("prefix".into(), prefix.clone()));
        out.push(fact(
            site,
            BridgeKind::Http,
            BoundaryRole::Provides,
            format!("MOUNT {prefix}"),
            call.owner,
            None,
            call.span,
            detail,
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn export(
        &self,
        site: &Site<'_>,
        start: u32,
        call: Option<&CallSite>,
        name: &ArgSel,
        via: &str,
        derived: bool,
        out: &mut Vec<BoundaryFact>,
    ) {
        // Expanded macro code: `Kw(<exported symbol>)` names the export itself; the
        // expansion is the compiler's output (an ABI fact, not derived package behaviour).
        let (literal, derived) = match (name, call) {
            (ArgSel::Kw(k), None) => (Some(k.clone()), false),
            (ArgSel::Kw(k), Some(c)) if !has_keyword(site.facts, c, k) => (Some(k.clone()), false),
            (sel, Some(c)) => (
                self.arg(site, c.callee_span, sel)
                    .and_then(|v| v.template.as_ref().and_then(Tpl::plain)),
                derived,
            ),
            _ => (None, derived),
        };
        let Some(exported) = literal.filter(|n| !n.is_empty()) else { return };
        let span = site
            .semantics
            .and_then(|s| s.expanded.iter().find(|e| e.span.start == start).map(|e| e.span))
            .or_else(|| call.map(|c| c.span))
            .unwrap_or(ByteSpan::new(start, start + 1));
        let decl = declaration_for(site.facts, span);
        let mut detail = common_detail(via, derived);
        detail.push(("definition".into(), "true".into()));
        detail.push(("linkage".into(), site.rec.language.as_str().to_string()));
        detail.push(("expanded".into(), "true".into()));
        out.push(fact(site, BridgeKind::CAbi, BoundaryRole::Provides, exported, None, decl, span, detail));
    }

    /// Verbs of a registration / request (`default` when the selected argument is absent).
    fn verbs(&self, site: &Site<'_>, call: &CallSite, verb: &VerbSel, default: &str) -> Vec<String> {
        match verb {
            VerbSel::Any => call_verbs(site, call),
            VerbSel::Const(v) => vec![verb_of(v).unwrap_or_else(|| "*".to_string())],
            VerbSel::Arg(sel) => {
                let Some(value) = self.arg(site, call.callee_span, sel) else {
                    return vec![default.to_string()];
                };
                let mut out: Vec<String> = match value.template.as_ref().and_then(Tpl::plain) {
                    Some(text) => text.split_whitespace().filter_map(verb_of).collect(),
                    None => verb_of(site.parsed.text(value.span).trim()).into_iter().collect(),
                };
                out.sort();
                out.dedup();
                if out.is_empty() {
                    vec!["*".to_string()]
                } else {
                    out
                }
            }
        }
    }

    /// Handler of a registration from its argument value.
    fn handler_of(&self, site: &Site<'_>, value: &ArgValue) -> Handler {
        let span = value.callable.unwrap_or(value.span);
        let text = site.parsed.text(span).trim().to_string();
        let decl = decl_at(site.facts, span).or_else(|| {
            let simple =
                !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$');
            if !simple {
                return None;
            }
            let hits: Vec<u32> = site
                .facts
                .declarations
                .iter()
                .enumerate()
                .filter(|(i, d)| {
                    Some(*i as u32) != site.facts.module_decl && d.kind.is_callable() && d.name == text
                })
                .map(|(i, _)| i as u32)
                .collect();
            (hits.len() == 1).then(|| hits[0])
        });
        Handler { decl, span, text }
    }
}

/// One file under recognition.
struct Site<'s> {
    rec: &'s FileRecord,
    facts: &'s FileFacts,
    semantics: Option<&'s FileSemantics>,
    parsed: &'s ParsedFile<'s>,
    lines: &'s trace_core::text::LineIndex,
}

/// The handler of a registration.
#[derive(Clone, Debug)]
struct Handler {
    decl: Option<u32>,
    span: ByteSpan,
    text: String,
}

#[allow(clippy::too_many_arguments)]
fn fact(
    site: &Site<'_>,
    kind: BridgeKind,
    role: BoundaryRole,
    name: String,
    owner: Option<u32>,
    decl: Option<u32>,
    span: ByteSpan,
    detail: Vec<(String, String)>,
) -> BoundaryFact {
    BoundaryFact {
        kind,
        role,
        name,
        owner,
        decl,
        span,
        line: site.lines.line1(span.start),
        detail,
    }
}

fn common_detail(via: &str, derived: bool) -> Vec<(String, String)> {
    let mut d = vec![(DERIVED.to_string(), derived.to_string())];
    if !via.is_empty() {
        d.push(("via".into(), via.to_string()));
    }
    d
}

/// Sort details, deduplicate, order facts like the syntax extractor.
fn finish(facts: &mut Vec<BoundaryFact>) {
    for f in facts.iter_mut() {
        f.detail.sort();
        f.detail.dedup_by(|a, b| a.0 == b.0);
    }
    facts.sort_by(|a, b| {
        (a.span.start, a.kind, a.role, &a.name, a.span.end, &a.detail).cmp(&(
            b.span.start,
            b.kind,
            b.role,
            &b.name,
            b.span.end,
            &b.detail,
        ))
    });
    facts.dedup();
}

/// HTTP methods a call names itself: the method token its callee is named after (`r.GET`),
/// else the method tokens among its literal arguments (`methods=["POST"]`), else any.
fn call_verbs(site: &Site<'_>, call: &CallSite) -> Vec<String> {
    if let Some(t) = call
        .member
        .as_deref()
        .and_then(trace_library::channels::http_method_token)
    {
        return vec![t.to_string()];
    }
    let mut out: Vec<String> = site
        .parsed
        .call_strings(call.callee_span, &|_| None)
        .iter()
        .filter_map(|s| s.value.template.as_ref().and_then(Tpl::plain))
        .flat_map(|t| {
            t.split_whitespace()
                .filter_map(trace_library::channels::http_method_token)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();
    out.sort();
    out.dedup();
    if out.is_empty() {
        vec!["*".to_string()]
    } else {
        out
    }
}

/// Library symbol / table origin of a behaviour (evidence text).
fn via_of(b: &CallBehaviour) -> String {
    b.symbol.clone().unwrap_or_else(|| b.source.as_str().to_string())
}

/// Syntax selector of a library selector.
pub(crate) fn arg_ref(sel: &ArgSel) -> Option<ArgRef> {
    Some(match sel {
        ArgSel::Pos(i) | ArgSel::Rest(i) | ArgSel::NamedBy(i) | ArgSel::Command(i) | ArgSel::Code(i) => {
            ArgRef::Pos(*i)
        }
        ArgSel::Kw(k) => ArgRef::Kw(k.clone()),
        ArgSel::PosOrKw(i, k) => ArgRef::PosOrKw(*i, k.clone()),
        ArgSel::Receiver => ArgRef::Receiver,
        ArgSel::Last => ArgRef::Last,
        ArgSel::Field { arg, field } => ArgRef::Field(*arg, field.clone()),
        ArgSel::Result | ArgSel::Member => return None,
    })
}

fn callee_span(facts: &FileFacts, start: u32) -> Option<ByteSpan> {
    facts
        .calls
        .iter()
        .find(|c| c.callee_span.start == start)
        .map(|c| c.callee_span)
}

fn has_keyword(facts: &FileFacts, call: &CallSite, key: &str) -> bool {
    facts
        .calls
        .iter()
        .position(|c| c.span == call.span && c.callee_span == call.callee_span)
        .and_then(|i| facts.call_detail(i))
        .is_some_and(|d| {
            d.arguments
                .iter()
                .any(|a| matches!(&a.slot, trace_core::facts::ArgSlot::Keyword(k) if k == key))
        })
}

/// The declaration (not `<module>`) whose span is `span` or starts at it.
fn decl_at(facts: &FileFacts, span: ByteSpan) -> Option<u32> {
    facts
        .declarations
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i as u32) != facts.module_decl)
        .find(|(_, d)| d.span.bytes == span || d.name_span == span || d.span.bytes.start == span.start)
        .map(|(i, _)| i as u32)
}

/// The decorated declaration following a decorator call: the nearest following declaration
/// that has decorators.
fn decorated_declaration(facts: &FileFacts, call: ByteSpan) -> Option<u32> {
    facts
        .declarations
        .iter()
        .enumerate()
        .filter(|(i, d)| {
            Some(*i as u32) != facts.module_decl && !d.decorators.is_empty() && d.name_span.start >= call.end
        })
        .min_by_key(|(_, d)| d.name_span.start)
        .map(|(i, _)| i as u32)
}

/// The declaration an expanded macro belongs to: the innermost declaration containing the
/// macro span, else the first declaration starting after it (attribute macros precede their
/// item).
fn declaration_for(facts: &FileFacts, span: ByteSpan) -> Option<u32> {
    let inner = facts
        .declarations
        .iter()
        .enumerate()
        .filter(|(i, d)| {
            Some(*i as u32) != facts.module_decl
                && d.span.bytes.encloses(span)
                && d.name_span.start >= span.start
        })
        .min_by_key(|(_, d)| d.span.bytes.len())
        .map(|(i, _)| i as u32);
    inner.or_else(|| {
        facts
            .declarations
            .iter()
            .enumerate()
            .filter(|(i, d)| Some(*i as u32) != facts.module_decl && d.span.bytes.start >= span.start)
            .min_by_key(|(_, d)| d.span.bytes.start)
            .map(|(i, _)| i as u32)
    })
}

/// `/pkg.Service/Method` (the gRPC wire path, gRPC over HTTP/2 spec) -> (package, service,
/// method).
fn rpc_key(key: &str) -> Option<(String, String, String)> {
    let key = key.trim_start_matches('/');
    let (full, method) = key.split_once('/')?;
    if method.is_empty() || method.contains('/') {
        return None;
    }
    let (package, service) = match full.rsplit_once('.') {
        Some((p, s)) => (p.to_string(), s.to_string()),
        None => (String::new(), full.to_string()),
    };
    (!service.is_empty()).then(|| (package, service, method.to_string()))
}

fn simple_name(s: &str) -> String {
    s.rsplit(['.', ':', '\\']).next().unwrap_or(s).to_string()
}

/// Whether a repository file declares annotation types, by content hash (cached across
/// incremental updates).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AnnotationFile {
    pub hash: trace_core::Hash32,
    pub declares: bool,
}

/// Java files of the repository and whether they declare annotation types (`@interface`,
/// read from the syntax tree); unchanged files reuse `previous`.
pub(crate) fn annotation_files(
    input: &BridgeInput<'_>,
    previous: Option<&BTreeMap<String, AnnotationFile>>,
) -> BTreeMap<String, AnnotationFile> {
    let mut out = BTreeMap::new();
    for (i, rec) in input.index.files.iter().enumerate() {
        if rec.language != Language::Java || rec.facts.is_none() {
            continue;
        }
        let known = previous
            .and_then(|p| p.get(&rec.path))
            .filter(|f| f.hash == rec.hash)
            .copied();
        let entry = match known {
            Some(f) => f,
            None => {
                let declares = input
                    .sources
                    .file(FileId(i as u32))
                    .map(|src| !trace_syntax::lower::annotation_types(Language::Java, &src.bytes).is_empty())
                    .unwrap_or(false);
                AnnotationFile {
                    hash: rec.hash,
                    declares,
                }
            }
        };
        out.insert(rec.path.clone(), entry);
    }
    out
}

#[cfg(test)]
#[path = "../../tests/unit/recognize/mod.rs"]
mod tests;
