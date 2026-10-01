//! Flow candidates into sites: flow / callback sites, merging into dispatch sites,
//! composition by receiver and test marks (child of [`crate::sites`]).

use super::*;

impl Generator<'_, '_> {
    /// Flow candidates: merged into matching dispatch or no_target sites, new flow/implicit
    /// sites, or flow callback sites. Composed candidates (`via`) are placed later.
    pub(super) fn flow(
        &mut self,
        candidates: &[FlowCandidate],
        no_target: &mut [Site],
        dispatch: &mut [Site],
    ) -> Placed {
        let index = self.index;
        let by_span: HashMap<(SymbolId, FileId, ByteSpan), usize> = no_target
            .iter()
            .enumerate()
            .map(|(i, s)| ((s.owner, s.at.file, s.at.bytes), i))
            .collect();
        let dispatch_at: HashMap<(SymbolId, FileId, ByteSpan), usize> = dispatch
            .iter()
            .enumerate()
            .filter(|(_, s)| s.via.is_none())
            .map(|(i, s)| ((s.owner, s.at.file, s.at.bytes), i))
            .collect();
        let mut placed = Placed {
            sites: Vec::new(),
            callbacks: Vec::new(),
            ids: vec![None; candidates.len()],
        };
        for (ci, c) in candidates.iter().enumerate() {
            if c.via.is_some() {
                continue;
            }
            if c.kind == CandidateKind::Flow
                && matches!(c.operation, SiteOperation::Call | SiteOperation::OverrideDispatch)
            {
                if let Some(&i) = dispatch_at.get(&(c.owner, c.file, c.span)) {
                    self.merge_into_dispatch(&mut dispatch[i], c);
                    placed.ids[ci] = Some(dispatch[i].id.clone());
                    continue;
                }
            }
            match c.kind {
                CandidateKind::Callback { arg } => {
                    if let Some((id, s)) = self.flow_callback(c, arg) {
                        placed.ids[ci] = Some(id);
                        placed.callbacks.extend(s);
                    }
                    continue;
                }
                CandidateKind::Flow if c.operation == SiteOperation::Call => {
                    if let Some(&i) = by_span.get(&(c.owner, c.file, c.span)) {
                        let site = &mut no_target[i];
                        let pool = site.candidates.clone();
                        let mut merged = pool.clone();
                        let vetoed = self.rejected.get(&site.id);
                        merged.extend(
                            c.candidates
                                .iter()
                                .copied()
                                .filter(|x| !vetoed.is_some_and(|v| v.contains(x))),
                        );
                        let (candidates, truncated) = finish_candidates(index, merged);
                        site.truncated_candidates |= truncated || c.bounded;
                        site.flow_candidates =
                            merge_subset(&site.flow_candidates, &c.candidates, &candidates);
                        site.field_only = merge_subset(&site.field_only, &c.field_only, &candidates);
                        // Name-pool members have their own (name) evidence.
                        let flow_tests: Vec<SymbolId> =
                            c.test_only.iter().copied().filter(|t| !pool.contains(t)).collect();
                        site.test_only = merge_subset(&site.test_only, &flow_tests, &candidates);
                        site.candidates = candidates;
                        placed.ids[ci] = Some(site.id.clone());
                        continue;
                    }
                }
                CandidateKind::Flow | CandidateKind::Implicit => {}
            }
            let category = if c.kind == CandidateKind::Implicit {
                SiteCategory::Implicit
            } else {
                SiteCategory::Flow
            };
            let operation = c.operation;
            let id = site_id(&[
                json!(category.as_str()),
                json!(self.uid(c.owner)),
                json!(index.file_path(c.file)),
                json!(c.span.start),
                json!(operation.as_str()),
            ]);
            placed.ids[ci] = Some(id.clone());
            if !self.fresh(&id) {
                // A declared-receiver rule row for a call value flow already placed as a new
                // site: its member joins that site (value-flow duplicates stay as they were).
                if ci >= self.rule_start && operation == SiteOperation::Call {
                    if let Some(s) = placed.sites.iter_mut().find(|s| s.id == id) {
                        merge_rule_candidate(index, s, c);
                    }
                }
                continue;
            }
            let (callee, line) = self.text_and_line(c.file, c.span, &c.callee, c.line);
            let (candidates, truncated) = finish_candidates(index, c.candidates.clone());
            let write = operation == SiteOperation::FieldWrite;
            // Field writes: only a single, unbounded, specific target becomes a site.
            if write && (candidates.len() != 1 || truncated || c.bounded || !c.field_only.is_empty()) {
                continue;
            }
            let mut s = site(
                id,
                category,
                c.owner,
                if write { EdgeKind::Writes } else { EdgeKind::Calls },
                Location {
                    file: c.file,
                    bytes: c.span,
                    line,
                },
                callee,
                candidates,
                truncated || c.bounded,
            );
            s.flow_candidates = s.candidates.clone();
            s.field_only = subset(&c.field_only, &s.candidates);
            s.test_only = subset(&c.test_only, &s.candidates);
            s.operation = Some(operation);
            s.receiver_exact = c.receiver_exact && !s.truncated_candidates;
            placed.sites.push(s);
        }
        placed
    }

    /// Class-hierarchy overrides of a stub (what value flow adds for unknown receivers): the
    /// methods of its name in its class's family, the class excluded.
    fn cha(&self, stub: SymbolId) -> Vec<SymbolId> {
        let Some(class) = self.hierarchy.class_of(self.index, stub) else {
            return Vec::new();
        };
        let name = self.index.symbol(stub).name.as_str();
        self.hierarchy
            .family(class)
            .into_iter()
            .filter(|&k| k != class)
            .filter_map(|k| self.hierarchy.method(k, name))
            .collect()
    }

    /// Union of a value-flow candidate at the same call into a dispatch site (module docs,
    /// step 4). Its targets are receiver evidence (`flow_candidates`) only when they come
    /// from known receivers: strong (not field-name) targets, not a class-hierarchy widening,
    /// complete (unbounded), and no proven receiver type decided the site already.
    fn merge_into_dispatch(&self, site: &mut Site, c: &FlowCandidate) {
        let index = self.index;
        let pool = site.candidates.clone();
        let widened = c.operation == SiteOperation::OverrideDispatch
            && site.declared_target.is_some_and(|stub| {
                let cha = self.cha(stub);
                !cha.is_empty() && cha.iter().all(|m| c.candidates.contains(m))
            });
        let strong: Vec<SymbolId> = c
            .candidates
            .iter()
            .copied()
            .filter(|t| !c.field_only.contains(t) && !c.test_only.contains(t))
            .collect();
        let mut merged = pool.clone();
        merged.extend(c.candidates.iter().copied());
        let (candidates, truncated) = finish_candidates(index, merged);
        site.truncated_candidates |= truncated || c.bounded;
        site.flow_candidates = if widened || site.receiver_exact || c.bounded {
            subset(&site.flow_candidates, &candidates)
        } else {
            merge_subset(&site.flow_candidates, &strong, &candidates)
        };
        let weak: Vec<SymbolId> = c.field_only.iter().copied().filter(|t| !pool.contains(t)).collect();
        site.field_only = merge_subset(&site.field_only, &weak, &candidates);
        let flow_tests: Vec<SymbolId> = c.test_only.iter().copied().filter(|t| !pool.contains(t)).collect();
        site.test_only = merge_subset(&site.test_only, &flow_tests, &candidates);
        site.candidates = candidates;
    }

    /// A flow callback candidate as a callback site (`None` for a duplicate id).
    fn flow_callback(&mut self, c: &FlowCandidate, arg: ByteSpan) -> Option<(SiteId, Option<Site>)> {
        let target = *c.candidates.first()?;
        let canonical = self.canonical_argument(c.file, c.span, arg);
        let id = site_id(&[
            json!("callback"),
            json!(self.uid(c.owner)),
            json!(self.uid(target)),
            json!(canonical.start),
        ]);
        if !self.fresh(&id) {
            return Some((id, None));
        }
        let fallback = self.index.symbol(target).name.clone();
        let (argument, _) = self.text_and_line(c.file, arg, &fallback, c.line);
        let (callee, line) = if c.callee.is_empty() {
            self.text_and_line(c.file, c.span, &fallback, c.line)
        } else {
            (c.callee.clone(), self.line(c.file, c.span.start, c.line))
        };
        let mut s = site(
            id.clone(),
            SiteCategory::Callback,
            c.owner,
            EdgeKind::InvokedCallback,
            Location {
                file: c.file,
                bytes: c.span,
                line,
            },
            callee,
            vec![target],
            false,
        );
        s.flow_candidates = vec![target];
        s.test_only = subset(&c.test_only, &s.candidates);
        s.argument = Some(argument);
        s.library = self.library_of(c.file, c.span, arg, None);
        Some((id, Some(s)))
    }

    /// A receiver-specialised flow candidate composed onto its parent site.
    pub(super) fn specialised(
        &mut self,
        c: &FlowCandidate,
        parent: &Site,
        parent_index: usize,
        through: SymbolId,
    ) -> Option<Site> {
        let id = site_id(&[
            json!(SiteCategory::Flow.as_str()),
            json!(self.uid(c.owner)),
            json!(self.index.file_path(c.file)),
            json!(parent.at.bytes.start),
            json!(SiteOperation::Call.as_str()),
            json!("via"),
            json!(self.uid(through)),
        ]);
        if !self.fresh(&id) {
            return None;
        }
        let (candidates, truncated) = finish_candidates(self.index, c.candidates.clone());
        let mut s = site(
            id,
            SiteCategory::Flow,
            c.owner,
            EdgeKind::Calls,
            parent.at,
            parent.callee.clone(),
            candidates,
            truncated || c.bounded,
        );
        s.flow_candidates = s.candidates.clone();
        s.test_only = subset(&c.test_only, &s.candidates);
        s.operation = Some(SiteOperation::Call);
        s.declared_target = Some(through);
        s.via = Some(parent_index as u32);
        s.argument = parent.argument.clone();
        Some(s)
    }

    /// Methods overriding `method` in subclasses of its class (non-stub, sorted by uid).
    pub(super) fn overrides(&self, method: SymbolId) -> Vec<SymbolId> {
        let Some(class) = self.hierarchy.class_of(self.index, method) else {
            return Vec::new();
        };
        let name = self.index.symbol(method).name.as_str();
        let mut out: Vec<SymbolId> = self
            .hierarchy
            .family(class)
            .into_iter()
            .filter(|&k| k != class)
            .filter_map(|k| self.hierarchy.method(k, name))
            .filter(|&m| m != method && !self.hierarchy.runs_nothing(self.index, m))
            .collect();
        out.sort_by(|a, b| self.uid(*a).cmp(self.uid(*b)));
        out.dedup();
        out
    }

    /// Compose dispatch / override resolution after every site kind (module docs, step 5).
    pub(super) fn compose(&mut self, out: &mut Vec<Site>, depth: &mut Vec<u8>) {
        let index = self.index;
        let mut implementations: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        let max_depth = trace_core::config::current().flow.max_compose_depth;
        let mut i = 0;
        while i < out.len() {
            let level = depth[i];
            let composable = level < max_depth
                && !matches!(out[i].category, SiteCategory::Dispatch | SiteCategory::NoTarget)
                && out[i].operation != Some(SiteOperation::FieldWrite);
            if !composable {
                i += 1;
                continue;
            }
            let parent = out[i].clone();
            for &c in &parent.candidates {
                if parent.test_only.contains(&c) {
                    continue;
                }
                let symbol = index.symbol(c);
                if symbol.is_stub && symbol.kind.is_callable() {
                    let impls = implementations
                        .entry(c)
                        .or_insert_with(|| self.hierarchy.implementations(index, c))
                        .clone();
                    if impls.is_empty() {
                        continue;
                    }
                    let id = site_id(&[
                        json!("dispatch"),
                        json!(self.uid(parent.owner)),
                        json!(self.uid(c)),
                        json!(parent.at.bytes.start),
                    ]);
                    if !self.fresh(&id) {
                        continue;
                    }
                    let (candidates, truncated) = finish_candidates(index, impls);
                    let mut s = site(
                        id,
                        SiteCategory::Dispatch,
                        parent.owner,
                        parent.activation,
                        parent.at,
                        parent.callee.clone(),
                        candidates,
                        truncated,
                    );
                    s.declared_target = Some(c);
                    s.via = Some(i as u32);
                    s.argument = parent.argument.clone();
                    out.push(s);
                    depth.push(level + 1);
                } else if parent.category == SiteCategory::Callback {
                    let overrides = self.overrides(c);
                    if overrides.is_empty() {
                        continue;
                    }
                    let id = site_id(&[
                        json!(SiteCategory::Flow.as_str()),
                        json!(self.uid(parent.owner)),
                        json!(index.file_path(parent.at.file)),
                        json!(parent.at.bytes.start),
                        json!(SiteOperation::OverrideDispatch.as_str()),
                        json!("via"),
                        json!(self.uid(c)),
                    ]);
                    if !self.fresh(&id) {
                        continue;
                    }
                    let (candidates, truncated) = finish_candidates(index, overrides);
                    let mut s = site(
                        id,
                        SiteCategory::Flow,
                        parent.owner,
                        EdgeKind::Calls,
                        parent.at,
                        parent.callee.clone(),
                        candidates,
                        truncated,
                    );
                    s.operation = Some(SiteOperation::OverrideDispatch);
                    s.declared_target = Some(c);
                    s.via = Some(i as u32);
                    s.argument = parent.argument.clone();
                    out.push(s);
                    depth.push(level + 1);
                }
            }
            i += 1;
        }
    }

    /// Product-owned sites: test-code candidates are test-only (module docs, step 6).
    pub(super) fn mark_tests(&self, out: &mut [Site]) {
        for s in out {
            if self.tests[s.owner.idx()] {
                s.test_only.clear();
                continue;
            }
            let marked: Vec<SymbolId> = s
                .candidates
                .iter()
                .copied()
                .filter(|c| s.test_only.contains(c) || self.tests[c.idx()])
                .collect();
            s.test_only = marked;
        }
    }
}

/// `existing ∪ new`, restricted to `candidates`, in candidate (uid) order.
/// Union of a declared-receiver rule candidate into a flow site placed for the same call.
/// A new member makes the site no longer `receiver_exact` (that property was computed for
/// the value-flow targets alone).
fn merge_rule_candidate(index: &Index, site: &mut Site, c: &FlowCandidate) {
    if c.candidates.iter().all(|t| site.candidates.contains(t)) {
        return;
    }
    let mut merged = site.candidates.clone();
    merged.extend(c.candidates.iter().copied());
    let (candidates, truncated) = finish_candidates(index, merged);
    site.truncated_candidates |= truncated || c.bounded;
    site.flow_candidates = merge_subset(&site.flow_candidates, &c.candidates, &candidates);
    site.field_only = subset(&site.field_only, &candidates);
    site.test_only = subset(&site.test_only, &candidates);
    site.candidates = candidates;
    site.receiver_exact = false;
}

fn merge_subset(existing: &[SymbolId], new: &[SymbolId], candidates: &[SymbolId]) -> Vec<SymbolId> {
    candidates
        .iter()
        .copied()
        .filter(|c| existing.contains(c) || new.contains(c))
        .collect()
}

/// `items` restricted to `candidates`, in candidate order.
pub(super) fn subset(items: &[SymbolId], candidates: &[SymbolId]) -> Vec<SymbolId> {
    merge_subset(items, &[], candidates)
}
