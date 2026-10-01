//! Accuracy guard (PLAN decision 13, DESIGN §1.14.6; owner speed): after any sequence of
//! edits the incremental graph must be identical to a full rebuild of the same files.
//! [`compare`] lists the differences over a normalised form: positional ids become symbol
//! uids / file paths / site ids, so two indexes of the same files compare equal exactly when
//! they say the same things.
//!
//! Compared: header versions and fingerprint, files (record, facts, semantics), configs,
//! omitted files, symbols, edges, unresolved sites, value references, sites, decisions,
//! bridges, library knowledge, support rows, diagnostics, stale files, library receivers and
//! backend runs
//! (backend, languages, file count, success).
//! Semantics are compared without their tool fingerprint (the analyzer build's cache key).
//! Volatile, never compared: build time and counters (`built_unix`, `full_builds`,
//! `incremental_updates`), file modification times, backend run timings / request and
//! queried-file counts / error texts, cache-hit counters and the persistence layout (journal
//! segments, phase-state encodings other than the library knowledge).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use trace_core::model::{FileId, Location, SymbolId};
use trace_core::Index;
use trace_library::LibraryKnowledge;

/// One difference between an incremental and a full index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Difference {
    pub area: &'static str,
    pub key: String,
    pub incremental: String,
    pub full: String,
}

/// Text of a side that lacks an entry.
const MISSING: &str = "<missing>";

/// Positional ids of one index in normalised form.
struct Names<'a> {
    index: &'a Index,
}

impl Names<'_> {
    fn symbol(&self, id: SymbolId) -> String {
        self.index
            .symbols
            .get(id.idx())
            .map_or_else(|| format!("<symbol {}>", id.0), |s| s.uid.clone())
    }

    fn symbols(&self, ids: &[SymbolId]) -> Value {
        Value::from(ids.iter().map(|&i| self.symbol(i)).collect::<Vec<_>>())
    }

    fn file(&self, id: FileId) -> String {
        self.index
            .files
            .get(id.idx())
            .map_or_else(|| format!("<file {}>", id.0), |f| f.path.clone())
    }

    fn location(&self, at: &Location) -> String {
        format!("{}:{}-{}@{}", self.file(at.file), at.bytes.start, at.bytes.end, at.line)
    }

    fn site(&self, i: u32) -> String {
        self.index
            .sites
            .get(i as usize)
            .map_or_else(|| format!("<site {i}>"), |s| s.id.0.clone())
    }
}

fn json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn set(v: &mut Value, key: &str, new: Value) {
    if let Some(map) = v.as_object_mut() {
        map.insert(key.to_string(), new);
    }
}

fn remove(v: &mut Value, key: &str) {
    if let Some(map) = v.as_object_mut() {
        map.remove(key);
    }
}

/// A multiset of normalised entries (text -> count).
type Bag = BTreeMap<String, usize>;

fn bag(items: impl IntoIterator<Item = String>) -> Bag {
    let mut out = Bag::new();
    for i in items {
        *out.entry(i).or_insert(0) += 1;
    }
    out
}

/// Keyed normalised entries.
type Keyed = BTreeMap<String, String>;

struct Normal {
    header: Keyed,
    files: Keyed,
    facts: Keyed,
    semantics: Keyed,
    configs: Keyed,
    omitted: Keyed,
    symbols: Keyed,
    edges: Bag,
    unresolved: Bag,
    value_refs: Bag,
    sites: Keyed,
    decisions: Keyed,
    bridges: Bag,
    knowledge: Keyed,
    support: Keyed,
    diagnostics: Bag,
    stale: Bag,
    library_receivers: Bag,
    backend_runs: Bag,
}

fn normalise(index: &Index) -> Normal {
    let n = Names { index };
    let h = &index.header;
    let header = Keyed::from([
        ("schema".to_string(), h.schema.to_string()),
        ("trace_version".to_string(), h.trace_version.clone()),
        ("root".to_string(), h.root.clone()),
        ("syntax_version".to_string(), h.syntax_version.to_string()),
        ("infer_version".to_string(), h.infer_version.to_string()),
        ("bridge_version".to_string(), h.bridge_version.to_string()),
        ("inventory_fingerprint".to_string(), h.inventory_fingerprint.to_hex()),
    ]);

    let mut files = Keyed::new();
    let mut facts = Keyed::new();
    let mut semantics = Keyed::new();
    for f in &index.files {
        let mut v = json(f);
        remove(&mut v, "mtime_ns");
        remove(&mut v, "first_symbol");
        files.insert(f.path.clone(), v.to_string());
        // Facts and semantics are compared by content hash (they are large); the key names
        // the file, the values say which side differs.
        facts.insert(f.path.clone(), trace_core::Hash32::of(json(&f.facts).to_string().as_bytes()).to_hex());
        // The tool fingerprint identifies the analyzer build (a cache key), not what it answered.
        let mut sem = json(&f.semantic);
        remove(&mut sem, "tool_fingerprint");
        semantics.insert(f.path.clone(), trace_core::Hash32::of(sem.to_string().as_bytes()).to_hex());
    }
    let configs = index
        .configs
        .iter()
        .map(|(p, hash)| (p.clone(), hash.to_hex()))
        .collect();
    let omitted = index
        .omitted
        .iter()
        .map(|o| (o.path.clone(), json(o).to_string()))
        .collect();

    let symbols = index
        .symbols
        .iter()
        .map(|s| {
            let mut v = json(s);
            remove(&mut v, "id");
            set(&mut v, "file", Value::from(n.file(s.file)));
            set(&mut v, "parent", Value::from(s.parent.map(|p| n.symbol(p))));
            (s.uid.clone(), v.to_string())
        })
        .collect();

    let edges = bag(index.edges.iter().map(|e| {
        let mut v = json(e);
        set(&mut v, "from", Value::from(n.symbol(e.from)));
        set(&mut v, "to", Value::from(n.symbol(e.to)));
        set(&mut v, "at", Value::from(n.location(&e.at)));
        v.to_string()
    }));
    let unresolved = bag(index.unresolved.iter().map(|u| {
        let mut v = json(u);
        set(&mut v, "owner", Value::from(u.owner.map(|o| n.symbol(o))));
        set(&mut v, "at", Value::from(n.location(&u.at)));
        set(&mut v, "candidates", n.symbols(&u.candidates));
        v.to_string()
    }));
    let value_refs = bag(index
        .value_refs
        .iter()
        .map(|r| format!("{} -> {}", n.location(&r.at), n.symbol(r.target))));

    let sites = index
        .sites
        .iter()
        .map(|s| {
            let mut v = json(s);
            set(&mut v, "owner", Value::from(n.symbol(s.owner)));
            set(&mut v, "declared_target", Value::from(s.declared_target.map(|t| n.symbol(t))));
            set(&mut v, "at", Value::from(n.location(&s.at)));
            set(&mut v, "candidates", n.symbols(&s.candidates));
            set(&mut v, "flow_candidates", n.symbols(&s.flow_candidates));
            set(&mut v, "field_only", n.symbols(&s.field_only));
            set(&mut v, "test_only", n.symbols(&s.test_only));
            set(&mut v, "via", Value::from(s.via.map(|i| n.site(i))));
            (s.id.0.clone(), v.to_string())
        })
        .collect();
    let decisions = index
        .decisions
        .iter()
        .map(|d| {
            let mut v = json(d);
            set(&mut v, "site", Value::from(n.site(d.site)));
            set(&mut v, "targets", n.symbols(&d.targets));
            (n.site(d.site), v.to_string())
        })
        .collect();
    let bridges = bag(index.bridges.iter().map(|b| {
        let mut v = json(b);
        set(&mut v, "from", Value::from(n.symbol(b.from)));
        set(&mut v, "to", Value::from(n.symbol(b.to)));
        set(&mut v, "from_at", Value::from(n.location(&b.from_at)));
        set(&mut v, "to_at", Value::from(n.location(&b.to_at)));
        set(&mut v, "contract", Value::from(b.contract.map(|f| n.file(f))));
        v.to_string()
    }));

    let knowledge = index
        .phase_state
        .iter()
        .find(|s| s.phase == crate::pipeline::update::LIBRARY_PHASE)
        .and_then(|s| postcard::from_bytes::<LibraryKnowledge>(&s.global).ok())
        .map(|k| {
            let mut out: Keyed = k
                .by_call
                .iter()
                .map(|((file, at), behaviour)| (format!("{file}@{at}"), json(behaviour).to_string()))
                .collect();
            // Work counters (time, cache hits) differ between an update and a rebuild by
            // design; the results are the counts.
            let mut stats = json(&k.stats);
            if let Value::Object(map) = &mut stats {
                map.remove("seconds");
                map.remove("cache_hits");
            }
            out.insert("<stats>".to_string(), stats.to_string());
            out
        })
        .unwrap_or_default();

    let support = index
        .support
        .iter()
        .map(|s| {
            let v = json(s);
            let key = v.get("language").map_or_else(|| v.to_string(), |l| l.to_string());
            (key, v.to_string())
        })
        .collect();
    let diagnostics = bag(index
        .diagnostics
        .iter()
        .map(|d| format!("{} {} {}", d.kind, d.file.as_deref().unwrap_or("-"), d.message)));
    let stale = bag(index.stale.iter().cloned());
    let library_receivers = bag(index
        .library_receivers
        .iter()
        .map(|r| format!("{} {}", n.location(&r.at), r.library)));
    let backend_runs = bag(index.backend_runs.iter().map(|r| {
        let languages: Vec<&str> = r.languages.iter().map(|l| l.as_str()).collect();
        format!("{} [{}] files={} ok={}", r.backend, languages.join(","), r.files, r.ok)
    }));

    Normal {
        header,
        files,
        facts,
        semantics,
        configs,
        omitted,
        symbols,
        edges,
        unresolved,
        value_refs,
        sites,
        decisions,
        bridges,
        knowledge,
        support,
        diagnostics,
        stale,
        library_receivers,
        backend_runs,
    }
}

fn keyed(area: &'static str, a: &Keyed, b: &Keyed, out: &mut Vec<Difference>) {
    for (k, va) in a {
        match b.get(k) {
            Some(vb) if vb == va => {}
            Some(vb) => out.push(Difference {
                area,
                key: k.clone(),
                incremental: va.clone(),
                full: vb.clone(),
            }),
            None => out.push(Difference {
                area,
                key: k.clone(),
                incremental: va.clone(),
                full: MISSING.to_string(),
            }),
        }
    }
    for (k, vb) in b {
        if !a.contains_key(k) {
            out.push(Difference {
                area,
                key: k.clone(),
                incremental: MISSING.to_string(),
                full: vb.clone(),
            });
        }
    }
}

fn bags(area: &'static str, a: &Bag, b: &Bag, out: &mut Vec<Difference>) {
    for (k, &ca) in a {
        let cb = b.get(k).copied().unwrap_or(0);
        if ca != cb {
            out.push(Difference {
                area,
                key: k.clone(),
                incremental: format!("x{ca}"),
                full: format!("x{cb}"),
            });
        }
    }
    for (k, &cb) in b {
        if !a.contains_key(k) {
            out.push(Difference {
                area,
                key: k.clone(),
                incremental: "x0".to_string(),
                full: format!("x{cb}"),
            });
        }
    }
}

/// Differences between `incremental` and `full` (empty = equivalent).
pub fn compare(incremental: &Index, full: &Index) -> Vec<Difference> {
    let a = normalise(incremental);
    let b = normalise(full);
    let mut out = Vec::new();
    keyed("header", &a.header, &b.header, &mut out);
    keyed("files", &a.files, &b.files, &mut out);
    keyed("facts", &a.facts, &b.facts, &mut out);
    keyed("semantics", &a.semantics, &b.semantics, &mut out);
    keyed("configs", &a.configs, &b.configs, &mut out);
    keyed("omitted", &a.omitted, &b.omitted, &mut out);
    keyed("symbols", &a.symbols, &b.symbols, &mut out);
    bags("edges", &a.edges, &b.edges, &mut out);
    bags("unresolved", &a.unresolved, &b.unresolved, &mut out);
    bags("value_refs", &a.value_refs, &b.value_refs, &mut out);
    keyed("sites", &a.sites, &b.sites, &mut out);
    keyed("decisions", &a.decisions, &b.decisions, &mut out);
    bags("bridges", &a.bridges, &b.bridges, &mut out);
    keyed("library", &a.knowledge, &b.knowledge, &mut out);
    keyed("support", &a.support, &b.support, &mut out);
    bags("diagnostics", &a.diagnostics, &b.diagnostics, &mut out);
    bags("stale", &a.stale, &b.stale, &mut out);
    bags("library_receivers", &a.library_receivers, &b.library_receivers, &mut out);
    bags("backend_runs", &a.backend_runs, &b.backend_runs, &mut out);
    out
}

#[cfg(test)]
#[path = "../tests/unit/equivalence.rs"]
mod tests;
