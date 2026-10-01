//! The site generator: dispatch, library-dispatch, callback and no-target sites with
//! their receiver evidence (child of [`crate::sites`]).

use super::*;

/// Flow candidates placed as sites.
pub(super) struct Placed {
    pub(super) sites: Vec<Site>,
    pub(super) callbacks: Vec<Site>,
    /// Per flow candidate: id of the site it landed in (merged, new or duplicate).
    pub(super) ids: Vec<Option<SiteId>>,
}

pub(super) struct Generator<'i, 's> {
    pub(super) index: &'i Index,
    pub(super) sources: &'s SourceStore<'s>,
    pub(super) hierarchy: &'i Hierarchy,
    pub(super) seen: HashSet<SiteId>,
    /// Test-code symbols (index = symbol id).
    pub(super) tests: Vec<bool>,
    pub(super) narrower: Narrower<'i>,
    pub(super) stats: SiteStats,
    /// file -> callback argument span -> index into `FileFacts::callbacks`.
    pub(super) callback_spans: HashMap<FileId, HashMap<ByteSpan, usize>>,
    /// C/C++ prototype -> its unique definition (`rule:c-prototype` edges).
    pub(super) prototypes: HashMap<SymbolId, SymbolId>,
    /// Index of the first declared-receiver rule candidate in the candidate list (value-flow
    /// candidates come first).
    pub(super) rule_start: usize,
    /// no_target site id -> candidates a language rule rejected (receiver shape, lexical
    /// scope, imports). Value flow never adds them back: a language rule is definitive.
    pub(super) rejected: HashMap<SiteId, HashSet<SymbolId>>,
    /// Library knowledge of every library call (`Site::library` of callback sites).
    pub(super) knowledge: &'i LibraryKnowledge,
    /// Callee spans of library calls (calls answered only outside the index).
    pub(super) library_calls: LibraryCalls,
    /// Receiver types from syntax (receiver evidence of dispatch sites).
    pub(super) types: Types<'i>,
    /// Calls whose receiver is only ever a library-created object (value flow).
    pub(super) library_receivers: HashSet<(FileId, ByteSpan)>,
}

/// What the receiver of a dispatch call proves about the implementation that runs (I-01).
#[derive(Clone, Debug, PartialEq, Eq)]
enum ReceiverEvidence {
    /// Nothing is known.
    None,
    /// The receiver's type is provably unrelated to every candidate.
    Unrelated,
    /// The receiver's type narrows the implementations to these (sorted by uid).
    Narrowed(Vec<SymbolId>),
    /// The receiver's type is proven (statically typed language, one concrete type) and
    /// exactly this implementation runs.
    Proven(SymbolId),
}

/// Languages whose compiler enforces declared types, so a syntax type of the receiver
/// proves which implementation runs (a language rule).
fn enforces_declared_types(language: Language) -> bool {
    trace_syntax::language_rules::rules(language).static_types
}

/// Records receiver evidence on a dispatch site (module docs, step 1).
fn apply_evidence(s: &mut Site, evidence: ReceiverEvidence) {
    match evidence {
        ReceiverEvidence::Proven(t) if s.candidates.contains(&t) => {
            s.receiver_exact = true;
            s.flow_candidates = vec![t];
        }
        ReceiverEvidence::Narrowed(targets) => {
            s.flow_candidates = subset(&targets, &s.candidates);
        }
        _ => {}
    }
}

impl Generator<'_, '_> {
    pub(super) fn uid(&self, id: SymbolId) -> &str {
        &self.index.symbol(id).uid
    }

    /// Exact text and 1-based line of a span; falls back to the given values if the
    /// source cannot be read (it was verified moments ago by the pipeline).
    pub(super) fn text_and_line(&self, file: FileId, span: ByteSpan, text: &str, line: u32) -> (String, u32) {
        match self.sources.file(file) {
            Ok(src) => (src.slice(span).into_owned(), src.lines.line1(span.start)),
            Err(_) => (text.to_string(), line),
        }
    }

    pub(super) fn line(&self, file: FileId, byte: u32, fallback: u32) -> u32 {
        self.sources
            .file(file)
            .map(|src| src.lines.line1(byte))
            .unwrap_or(fallback)
    }

    pub(super) fn fresh(&mut self, id: &SiteId) -> bool {
        self.seen.insert(id.clone())
    }

    /// `Site::library` of a callback site: what the library call whose callee span is
    /// `callee` does with the argument at `arg` (`None` when the receiving call is not a
    /// library call). The argument's position / keyword come from its `CallbackArg`
    /// (`cb`, else the one at `arg`), else from the call's argument slots.
    pub(super) fn library_of(
        &mut self,
        file: FileId,
        callee: ByteSpan,
        arg: ByteSpan,
        cb: Option<&CallbackArg>,
    ) -> Option<LibraryBehaviour> {
        let index = self.index;
        let facts = index.file(file).facts.as_ref();
        let recorded = |c: &CallbackArg| (c.index, c.keyword.clone());
        let cb: Option<(Option<u32>, Option<String>)> = match cb {
            Some(c) => Some(recorded(c)),
            None => facts
                .and_then(|f| {
                    f.callbacks.iter().find(|c| {
                        c.call_callee_span == callee
                            && (c.arg_span == arg || arg.encloses(c.arg_span) || c.arg_span.encloses(arg))
                    })
                })
                .map(recorded),
        };
        let call = facts.and_then(|f| {
            let i = f.calls.iter().position(|c| c.callee_span == callee)?;
            Some((&f.calls[i], f.call_detail(i)))
        });
        let slots = call.and_then(|(_, d)| d).map(|d| &d.arguments);
        let positional = match (slots, call) {
            (Some(args), _) => args
                .iter()
                .filter(|a| matches!(a.slot, ArgSlot::Positional { .. }))
                .count(),
            (None, Some((c, _))) => c.arg_count as usize,
            (None, None) => 0,
        };
        let (position, keyword): (Option<usize>, Option<String>) = match cb {
            Some((index, keyword)) if index.is_some() || keyword.is_some() => {
                (index.map(|i| i as usize), keyword)
            }
            _ => match slots.and_then(|args| args.iter().find(|a| a.span.encloses(arg))) {
                Some(a) => match &a.slot {
                    ArgSlot::Positional { index, exact: true } => (Some(*index as usize), None),
                    ArgSlot::Keyword(k) => (None, Some(k.clone())),
                    _ => (None, None),
                },
                None => (None, None),
            },
        };
        // The receiving call's callee is itself a call: a decorator application
        // (`app.route("/")(f)`), which applies the inner call's `Decorates` effects.
        let decorator_application =
            facts.is_some_and(|f| f.calls.iter().any(|c: &CallSite| c.span == callee));
        let behaviour = behaviour_for(
            index,
            &self.library_calls,
            self.knowledge,
            file,
            callee,
            SiteArg {
                index: position,
                keyword: keyword.as_deref(),
                positional,
                decorator_application,
            },
        );
        if behaviour.is_some() {
            self.stats.library_sites += 1;
        }
        behaviour
    }

    pub(super) fn dispatch(&mut self) -> Vec<Site> {
        let index = self.index;
        let mut out = Vec::new();
        let mut implementations: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        for e in &index.edges {
            // Reference and family kinds (reads, writes, imports, re-exports, overrides,
            // implements) are not executions of the stub.
            if trace_core::tiers::REFERENCE.contains(e.kind) || trace_core::tiers::FAMILY.contains(e.kind) {
                continue;
            }
            let stub = index.symbol(e.to);
            if !(stub.is_stub && stub.kind.is_callable()) {
                continue;
            }
            let impls = implementations
                .entry(e.to)
                .or_insert_with(|| self.hierarchy.implementations(index, e.to));
            if impls.is_empty() {
                continue;
            }
            let id = site_id(&[
                json!("dispatch"),
                json!(self.uid(e.from)),
                json!(self.uid(e.to)),
                json!(e.at.bytes.start),
            ]);
            if !self.fresh(&id) {
                continue;
            }
            // The call whose callee span is the edge's span (`a.f().g` starts where `a.f`
            // does: the smallest callee at the start would be the inner call).
            let call = call_with_callee(index, e.at.file, e.at.bytes)
                .or_else(|| call_at(index, e.at.file, e.at.bytes.start));
            let (span, callee, line, member) = match call {
                Some(c) => (
                    c.callee_span,
                    c.callee.clone(),
                    self.line(e.at.file, c.callee_span.start, c.line),
                    c.member.clone(),
                ),
                None if !e.at.bytes.is_empty() => {
                    let (text, line) = self.text_and_line(e.at.file, e.at.bytes, &stub.name, e.at.line);
                    (e.at.bytes, text, line, None)
                }
                None => (e.at.bytes, stub.name.clone(), e.at.line, None),
            };
            let (candidates, truncated) = finish_candidates(index, impls.clone());
            let evidence =
                self.receiver_evidence(e.at.file, span, member.as_deref(), &stub.name, &candidates);
            let mut s = site(
                id,
                SiteCategory::Dispatch,
                e.from,
                e.kind,
                Location {
                    file: e.at.file,
                    bytes: span,
                    line,
                },
                callee,
                candidates,
                truncated,
            );
            s.declared_target = Some(e.to);
            // The server resolved the call to the abstract member: a receiver type that looks
            // unrelated to every implementation is no evidence here (only for library
            // dispatch, where it rules the site out).
            if evidence != ReceiverEvidence::Unrelated {
                apply_evidence(&mut s, evidence);
            }
            out.push(s);
        }
        out
    }

    /// Member identifier span of the call whose callee span is `callee`: the member access
    /// ending at the callee end, else the callee's last `member.len()` bytes.
    fn member_span(&self, file: FileId, callee: ByteSpan, member: Option<&str>) -> Option<ByteSpan> {
        let facts = self.index.file(file).facts.as_ref()?;
        // Member accesses are sorted by span: scan those starting inside the callee.
        let first = facts.member_accesses.partition_point(|m| m.span.start < callee.start);
        if let Some(m) = facts.member_accesses[first..]
            .iter()
            .take_while(|m| m.span.start < callee.end)
            .find(|m| m.span.end == callee.end && callee.encloses(m.span))
        {
            return Some(m.span);
        }
        let member = member.filter(|m| !m.is_empty())?;
        let len = member.len() as u32;
        (callee.len() > len).then(|| ByteSpan::new(callee.end - len, callee.end))
    }

    /// The implementations a receiver of one of `types` can run for member `name`: for
    /// every type and each of its subtypes, the first definer of `name` in its base order
    /// (abstract definers run nothing). Sorted by uid.
    fn dispatch_targets(&self, types: &[SymbolId], name: &str) -> Vec<SymbolId> {
        let index = self.index;
        let hierarchy = self.hierarchy;
        let mut out: Vec<SymbolId> = Vec::new();
        for &t in types {
            let mut family: Vec<SymbolId> = hierarchy.family(t).into_iter().collect();
            family.sort_unstable();
            for k in family {
                let runs = hierarchy.mro(k).into_iter().find_map(|c| hierarchy.method(c, name));
                if let Some(m) = runs.filter(|&m| !hierarchy.runs_nothing(index, m)) {
                    out.push(m);
                }
            }
        }
        out.sort_by(|a, b| index.symbol(*a).uid.cmp(&index.symbol(*b).uid));
        out.dedup();
        out
    }

    /// Receiver evidence of a dispatch call (module docs, step 1): the syntax type of the
    /// receiver of the call whose callee span is `callee` in `file`, against the site's
    /// implementation `candidates` of member `name`.
    fn receiver_evidence(
        &mut self,
        file: FileId,
        callee: ByteSpan,
        member: Option<&str>,
        name: &str,
        candidates: &[SymbolId],
    ) -> ReceiverEvidence {
        if candidates.is_empty() {
            return ReceiverEvidence::None;
        }
        let Some(span) = self.member_span(file, callee, member.or(Some(name))) else {
            return ReceiverEvidence::None;
        };
        let ty = self.types.receiver_type(file, span);
        if ty == ReceiverType::Unknown {
            return ReceiverEvidence::None;
        }
        if self.types.unrelated_to_family(&ty, candidates) {
            return ReceiverEvidence::Unrelated;
        }
        let ReceiverType::Index(types) = ty else {
            return ReceiverEvidence::None;
        };
        let targets = self.dispatch_targets(&types, name);
        if targets.is_empty() || !targets.iter().all(|t| candidates.contains(t)) {
            return ReceiverEvidence::None;
        }
        let index = self.index;
        if let ([only], [ty]) = (targets.as_slice(), types.as_slice()) {
            let concrete = index.symbol(*ty).kind != SymbolKind::Interface;
            let own = self
                .hierarchy
                .mro(*ty)
                .into_iter()
                .find_map(|c| self.hierarchy.method(c, name));
            if concrete && own == Some(*only) && enforces_declared_types(index.file(file).language) {
                return ReceiverEvidence::Proven(*only);
            }
        }
        if targets.len() == 1 || targets.len() < candidates.len() {
            ReceiverEvidence::Narrowed(targets)
        } else {
            ReceiverEvidence::None
        }
    }

    /// The executing symbol of the call whose callee span is `callee` in `file` (the
    /// `<module>` symbol at module level), else the innermost symbol containing it.
    fn call_owner(&self, file: FileId, callee: ByteSpan) -> Option<SymbolId> {
        let record = self.index.file(file);
        let facts = record.facts.as_ref()?;
        match facts.calls.iter().find(|c| c.callee_span == callee) {
            Some(c) => facts.executing_owner(c.owner).and_then(|d| record.symbol_of_decl(d)),
            None => self.index.symbol_at(file, callee.start),
        }
    }

    /// Library-declared dispatch sites (module docs, step 1, I-02).
    pub(super) fn library_dispatch(&mut self) -> Vec<Site> {
        let index = self.index;
        let mut out = Vec::new();
        // Languages whose server answered implementation requests for library members.
        let capable: HashSet<Language> = index
            .files
            .iter()
            .filter(|f| f.semantic.as_ref().is_some_and(|s| !s.library_dispatch.is_empty()))
            .map(|f| f.language)
            .collect();
        if !capable.is_empty() {
            let by_uid: HashMap<&str, SymbolId> =
                index.symbols.iter().map(|s| (s.uid.as_str(), s.id)).collect();
            for (fi, record) in index.files.iter().enumerate() {
                let Some(sem) = &record.semantic else { continue };
                let file = FileId(fi as u32);
                for d in &sem.library_dispatch {
                    let Some(owner) = record.symbol_of_decl(d.owner) else {
                        continue;
                    };
                    let implementations: Vec<SymbolId> = d
                        .implementations
                        .iter()
                        .filter_map(|u| by_uid.get(u.as_str()).copied())
                        .collect();
                    let symbol = d.library_symbol.clone().unwrap_or_default();
                    if let Some(s) = self.library_site(file, owner, d.at, d.line, symbol, implementations) {
                        out.push(s);
                    }
                }
            }
        }
        // Family fallback for servers without implementation answers.
        if self.hierarchy.library_members.is_empty() {
            return out;
        }
        for (fi, record) in index.files.iter().enumerate() {
            if capable.contains(&record.language) {
                continue;
            }
            let Some(sem) = &record.semantic else { continue };
            let file = FileId(fi as u32);
            for call in &sem.library_calls {
                let Some(symbol) = call.symbol.as_deref() else { continue };
                let implementations = self.hierarchy.library_implementations(symbol).to_vec();
                if implementations.is_empty() {
                    continue;
                }
                let Some(owner) = self.call_owner(file, call.at) else { continue };
                if let Some(s) =
                    self.library_site(file, owner, call.at, call.line, symbol.to_string(), implementations)
                {
                    out.push(s);
                }
            }
        }
        out
    }

    /// One library-declared dispatch site at the call whose callee span is `at` (`None`
    /// without implementations, for a library-created or unrelated receiver, or a
    /// duplicate id).
    fn library_site(
        &mut self,
        file: FileId,
        owner: SymbolId,
        at: ByteSpan,
        line: u32,
        symbol: String,
        implementations: Vec<SymbolId>,
    ) -> Option<Site> {
        let index = self.index;
        let implementations: Vec<SymbolId> = implementations
            .into_iter()
            .filter(|&m| !self.hierarchy.runs_nothing(index, m) && index.symbol(m).kind.is_callable())
            .collect();
        if implementations.is_empty() || self.library_receivers.contains(&(file, at)) {
            return None;
        }
        let id = site_id(&[
            json!("dispatch"),
            json!(self.uid(owner)),
            json!(format!("lib:{symbol}")),
            json!(at.start),
        ]);
        if !self.fresh(&id) {
            return None;
        }
        let (callee, line, member) = match call_with_callee(index, file, at) {
            Some(c) => (c.callee.clone(), self.line(file, at.start, c.line), c.member.clone()),
            None => {
                let (text, line) = self.text_and_line(file, at, &symbol, line);
                (text, line, None)
            }
        };
        let (candidates, truncated) = finish_candidates(index, implementations);
        let name = index.symbol(candidates[0]).name.clone();
        let evidence = self.receiver_evidence(file, at, member.as_deref(), &name, &candidates);
        if evidence == ReceiverEvidence::Unrelated {
            return None;
        }
        let declared = if symbol.is_empty() { callee.clone() } else { symbol };
        let mut s = site(
            id,
            SiteCategory::Dispatch,
            owner,
            EdgeKind::Calls,
            Location {
                file,
                bytes: at,
                line,
            },
            callee,
            candidates,
            truncated,
        );
        s.declared_library = Some(declared);
        apply_evidence(&mut s, evidence);
        Some(s)
    }

    /// The argument span of the syntax `CallbackArg` of the call whose callee span is
    /// `callee` that `arg` lies in or encloses (the smallest), else `arg` itself: one
    /// canonical span per argument for callback site ids (I-23).
    pub(super) fn canonical_argument(&self, file: FileId, callee: ByteSpan, arg: ByteSpan) -> ByteSpan {
        self.index
            .file(file)
            .facts
            .as_ref()
            .and_then(|f| {
                f.callbacks
                    .iter()
                    .filter(|c| {
                        c.call_callee_span == callee
                            && (c.arg_span == arg || c.arg_span.encloses(arg) || arg.encloses(c.arg_span))
                    })
                    .min_by_key(|c| (c.arg_span.len(), c.arg_span.start))
                    .map(|c| c.arg_span)
            })
            .unwrap_or(arg)
    }

    pub(super) fn callbacks(&mut self) -> Vec<Site> {
        let index = self.index;
        let mut out = Vec::new();
        for e in &index.edges {
            if e.kind != EdgeKind::PassesCallback {
                continue;
            }
            let Some(facts) = index.file(e.at.file).facts.as_ref() else {
                continue;
            };
            let exact = self
                .callback_spans
                .entry(e.at.file)
                .or_insert_with(|| {
                    let mut map = HashMap::new();
                    for (i, c) in facts.callbacks.iter().enumerate() {
                        map.entry(c.arg_span).or_insert(i);
                    }
                    map
                })
                .get(&e.at.bytes)
                .map(|&i| &facts.callbacks[i]);
            let arg = exact.or_else(|| {
                facts
                    .callbacks
                    .iter()
                    .filter(|c| c.arg_span.contains(e.at.bytes.start))
                    .min_by_key(|c| c.arg_span.len())
            });
            let Some(arg) = arg else { continue };
            // One site per (owner, target, argument): every edge inside the argument shares
            // the argument's id (I-23).
            let id = site_id(&[
                json!("callback"),
                json!(self.uid(e.from)),
                json!(self.uid(e.to)),
                json!(arg.arg_span.start),
            ]);
            if !self.fresh(&id) {
                continue;
            }
            let line = self.line(e.at.file, arg.call_callee_span.start, e.at.line);
            let library = self.library_of(e.at.file, arg.call_callee_span, arg.arg_span, Some(arg));
            let mut s = site(
                id,
                SiteCategory::Callback,
                e.from,
                EdgeKind::InvokedCallback,
                Location {
                    file: e.at.file,
                    bytes: arg.call_callee_span,
                    line,
                },
                arg.callee.clone(),
                vec![e.to],
                false,
            );
            s.argument = Some(arg.argument.clone());
            s.library = library;
            out.push(s);
        }
        out
    }

    pub(super) fn no_target(&mut self) -> Vec<Site> {
        let index = self.index;
        let hierarchy = self.hierarchy;
        let mut out = Vec::new();
        for u in &index.unresolved {
            if !u.kind.is_blind() {
                continue;
            }
            let Some(owner) = u.owner else { continue };
            let syntax = self.narrower.call_at(u.at.file, u.at.bytes).map(|(_, c)| c);
            let name = match syntax {
                Some(c) => c.member.as_deref(),
                None => member_of(&u.callee),
            };
            let Some(name) = name else { continue };
            // The by-name pool is sorted by uid: collection stops after MAX_CANDIDATES + 1
            // survivors, which yields exactly the sorted-and-truncated full pool.
            let Some(pool) = hierarchy.functions.get(name) else {
                continue;
            };
            if pool.iter().all(|&c| c == owner) {
                continue;
            }
            let shape = self.narrower.shape(u, owner, name);
            let narrowed = self.narrower.narrow(&shape, pool, MAX_CANDIDATES + 1);
            let dropped = narrowed.dropped_by_import.len()
                + narrowed.dropped_by_receiver.len()
                + narrowed.dropped_by_scope.len()
                + narrowed.dropped_by_arity.len()
                + narrowed.dropped_by_visibility.len();
            self.stats.dropped_by_visibility += narrowed.dropped_by_visibility.len() as u64;
            self.stats.dropped_by_import += narrowed.dropped_by_import.len() as u64;
            self.stats.dropped_by_receiver += narrowed.dropped_by_receiver.len() as u64;
            self.stats.dropped_by_scope += narrowed.dropped_by_scope.len() as u64;
            self.stats.dropped_by_arity += narrowed.dropped_by_arity.len() as u64;
            self.stats.weak_candidates += narrowed.weak.len() as u64;
            if narrowed.kept.is_empty() && dropped == 0 {
                continue;
            }
            let id = site_id(&[
                json!("no_target"),
                json!(self.uid(owner)),
                json!(index.file_path(u.at.file)),
                json!(u.at.bytes.start),
            ]);
            if !self.fresh(&id) {
                continue;
            }
            // Visibility drops are not rejections: the local binding may hold the function
            // (value flow may deliver it).
            let rejected: HashSet<SymbolId> = narrowed
                .dropped_by_receiver
                .iter()
                .chain(&narrowed.dropped_by_scope)
                .chain(&narrowed.dropped_by_arity)
                .chain(&narrowed.dropped_by_import)
                .copied()
                .collect();
            if !rejected.is_empty() {
                self.rejected.insert(id.clone(), rejected);
            }
            // A C/C++ prototype and its unique definition are one entity (proven
            // `rule:c-prototype` edge): the call runs the definition.
            let mut kept = narrowed.kept;
            if !self.prototypes.is_empty() {
                let all: HashSet<SymbolId> = kept.iter().copied().collect();
                kept.retain(|c| !self.prototypes.get(c).is_some_and(|d| all.contains(d)));
            }
            let (candidates, truncated) = finish_candidates(index, kept);
            let mut s = site(
                id,
                SiteCategory::NoTarget,
                owner,
                EdgeKind::Calls,
                u.at,
                u.callee.clone(),
                candidates,
                truncated || narrowed.truncated,
            );
            s.field_only = subset(&narrowed.weak, &s.candidates);
            // Arity rulings remove candidates; they are never evidence FOR the candidates
            // left (the call may run a same-named method of a library object, and a
            // decorated candidate is never ruled on): after an arity elimination every
            // remaining candidate has only its name (`field_only`), so uniqueness produced by
            // the elimination never decides the site.
            if !narrowed.dropped_by_arity.is_empty() {
                s.field_only = s.candidates.clone();
            }
            out.push(s);
        }
        out
    }
}
