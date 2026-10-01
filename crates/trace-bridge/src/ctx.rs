//! Shared lookups over an assembled index: boundary facts grouped by kind, symbol ends of a
//! fact, module and import resolution (syntax facts only), and bridge emission helpers.

use std::collections::{HashMap, HashSet};

use trace_core::facts::{BoundaryFact, BoundaryRole, FileFacts};
use trace_core::model::{
    Bridge, BridgeKind, ByteSpan, Diagnostic, Edge, FileId, Index, Location, Provider, Resolution, SymbolId,
    Tier,
};
use trace_core::source::SourceStore;
use trace_core::Language;

/// Upper bound on emitted bridges (a pathological repository cannot blow up the index).
pub(crate) const MAX_BRIDGES: usize = 200_000;

/// One end of a bridge: the symbol and the evidence location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct End {
    pub sym: SymbolId,
    pub at: Location,
}

/// A boundary fact with its file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fact<'a> {
    pub file: FileId,
    pub language: Language,
    pub fact: &'a BoundaryFact,
}

impl<'a> Fact<'a> {
    pub fn detail(&self, key: &str) -> Option<&'a str> {
        self.fact
            .detail
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    pub fn flag(&self, key: &str) -> bool {
        self.detail(key) == Some("true")
    }
    pub fn name(&self) -> &'a str {
        &self.fact.name
    }
    pub fn at(&self) -> Location {
        Location {
            file: self.file,
            bytes: self.fact.span,
            line: self.fact.line,
        }
    }
}

/// Everything the matchers share.
pub(crate) struct Ctx<'a> {
    pub index: &'a Index,
    pub sources: &'a SourceStore<'a>,
    pub facts: Vec<Fact<'a>>,
    /// Dotted module suffixes of Python source files (`a/b/c.py` -> `a.b.c`, `b.c`, `c`).
    pub py_modules: HashMap<String, Vec<FileId>>,
    /// Full dotted module path of Python source files from the repository root.
    pub py_full: HashMap<String, FileId>,
    pub edges_by_file: HashMap<FileId, Vec<&'a Edge>>,
    pub by_qualified: HashMap<&'a str, Vec<SymbolId>>,
    pub children: HashMap<SymbolId, Vec<SymbolId>>,
    pub by_container: HashMap<&'a str, Vec<SymbolId>>,
    pub unresolved_spans: HashSet<(FileId, u32, u32)>,
    pub bridges: Vec<Bridge>,
    pub diagnostics: Vec<Diagnostic>,
    pub unresolved_ends: usize,
}

impl<'a> Ctx<'a> {
    /// Matching context over the endpoints of every file (`recognize`).
    pub fn new(
        index: &'a Index,
        sources: &'a SourceStore<'a>,
        endpoints: &'a [crate::recognize::Endpoint],
    ) -> Self {
        let facts: Vec<Fact<'a>> = endpoints
            .iter()
            .map(|e| Fact {
                file: e.file,
                language: e.language,
                fact: &e.fact,
            })
            .collect();
        let mut py_modules: HashMap<String, Vec<FileId>> = HashMap::new();
        let mut py_full = HashMap::new();
        for (i, rec) in index.files.iter().enumerate() {
            let file = FileId(i as u32);
            if rec.language == Language::Python && rec.path.ends_with(".py") {
                let module = python_module_of_path(&rec.path);
                if !module.is_empty() {
                    py_full.insert(module.clone(), file);
                    let segs: Vec<&str> = module.split('.').collect();
                    for k in 0..segs.len() {
                        py_modules.entry(segs[k..].join(".")).or_default().push(file);
                    }
                }
            }
        }
        let mut edges_by_file: HashMap<FileId, Vec<&Edge>> = HashMap::new();
        for e in &index.edges {
            edges_by_file.entry(e.at.file).or_default().push(e);
        }
        let mut by_qualified: HashMap<&str, Vec<SymbolId>> = HashMap::new();
        let mut children: HashMap<SymbolId, Vec<SymbolId>> = HashMap::new();
        let mut by_container: HashMap<&str, Vec<SymbolId>> = HashMap::new();
        for s in &index.symbols {
            if s.is_synthetic() {
                continue;
            }
            by_qualified.entry(s.qualified_name.as_str()).or_default().push(s.id);
            if let Some(p) = s.parent {
                children.entry(p).or_default().push(s.id);
            }
            if let Some(c) = &s.container {
                by_container.entry(c.as_str()).or_default().push(s.id);
            }
        }
        let unresolved_spans = index
            .unresolved
            .iter()
            .map(|u| (u.at.file, u.at.bytes.start, u.at.bytes.end))
            .collect();
        Ctx {
            index,
            sources,
            facts,
            py_modules,
            py_full,
            edges_by_file,
            by_qualified,
            children,
            by_container,
            unresolved_spans,
            bridges: Vec::new(),
            diagnostics: Vec::new(),
            unresolved_ends: 0,
        }
    }

    pub fn of(&self, kind: BridgeKind, role: BoundaryRole) -> Vec<Fact<'a>> {
        self.facts
            .iter()
            .filter(|f| f.fact.kind == kind && f.fact.role == role)
            .copied()
            .collect()
    }

    pub fn file_facts(&self, file: FileId) -> Option<&'a FileFacts> {
        self.index.files.get(file.idx())?.facts.as_ref()
    }

    pub fn path(&self, file: FileId) -> &'a str {
        &self.index.files[file.idx()].path
    }

    pub fn language(&self, file: FileId) -> Language {
        self.index.files[file.idx()].language
    }

    /// Symbol of declaration `decl` in `file`.
    pub fn decl_symbol(&self, file: FileId, decl: u32) -> Option<SymbolId> {
        self.index.files.get(file.idx())?.symbol_of_decl(decl)
    }

    /// The synthetic `<module>` symbol of a file (module-level code), if present.
    pub fn module_symbol(&self, file: FileId) -> Option<SymbolId> {
        let facts = self.file_facts(file)?;
        let d = facts.executing_owner(None)?;
        self.decl_symbol(file, d)
    }

    /// Executing symbol for a syntax owner: the owner, else `<module>`, else the innermost
    /// symbol at `byte` (facts without a module declaration).
    pub fn executing(&self, file: FileId, owner: Option<u32>, byte: u32) -> Option<SymbolId> {
        let facts = self.file_facts(file)?;
        facts
            .executing_owner(owner)
            .and_then(|d| self.decl_symbol(file, d))
            .or_else(|| self.index.symbol_at(file, byte))
    }

    /// The symbol a fact is about: its declaration, else its executing owner.
    pub fn end_of(&mut self, f: &Fact<'_>) -> Option<End> {
        let sym = f
            .fact
            .decl
            .and_then(|d| self.decl_symbol(f.file, d))
            .or_else(|| self.executing(f.file, f.fact.owner, f.fact.span.start));
        if sym.is_none() {
            self.unresolved_ends += 1;
        }
        Some(End {
            sym: sym?,
            at: f.at(),
        })
    }

    /// End at a symbol's own declaration span.
    pub fn symbol_end(&self, sym: SymbolId) -> End {
        let s = self.index.symbol(sym);
        End {
            sym,
            at: Location {
                file: s.file,
                bytes: s.name_span,
                line: s.span.start_line,
            },
        }
    }

    /// Proven edge whose evidence lies inside `span` of `file` (the language server resolved
    /// the value there): its target.
    pub fn edge_target_in(&self, file: FileId, span: ByteSpan) -> Option<SymbolId> {
        let edges = self.edges_by_file.get(&file)?;
        edges
            .iter()
            .filter(|e| span.encloses(e.at.bytes))
            .min_by_key(|e| (e.at.bytes.start, e.to))
            .map(|e| e.to)
    }

    /// Non-synthetic callable/type symbols with this exact qualified name.
    pub fn qualified(&self, name: &str) -> Vec<SymbolId> {
        self.by_qualified.get(name).cloned().unwrap_or_default()
    }

    /// Symbols of `file` with this qualified name.
    pub fn qualified_in(&self, file: FileId, name: &str) -> Vec<SymbolId> {
        self.index
            .symbols_of(file)
            .iter()
            .filter(|s| !s.is_synthetic() && s.qualified_name == name)
            .map(|s| s.id)
            .collect()
    }

    pub fn push(&mut self, bridge: Bridge) {
        if bridge.from == bridge.to {
            return;
        }
        if self.bridges.len() >= MAX_BRIDGES {
            return;
        }
        self.bridges.push(bridge);
    }

    /// Emit one row per candidate: `unique_tier` when there is exactly one, else possible.
    #[allow(clippy::too_many_arguments)]
    pub fn emit(
        &mut self,
        kind: BridgeKind,
        from: End,
        candidates: &[End],
        unique_tier: Tier,
        provider: Provider,
        resolution: Resolution,
        label: &str,
        assumptions: &[String],
        contract: Option<FileId>,
    ) {
        let mut cands: Vec<End> = candidates.to_vec();
        cands.sort_by_key(|c| (c.sym, c.at));
        cands.dedup_by(|a, b| a.sym == b.sym && a.at == b.at);
        let n = cands.len() as u32;
        for c in cands {
            let tier = if n == 1 { unique_tier } else { Tier::Possible };
            let mut assumptions = assumptions.to_vec();
            if n > 1 {
                assumptions.push(format!("{n} candidates match; each is possible"));
            }
            self.push(Bridge {
                kind,
                tier: clamp(kind, tier),
                from: from.sym,
                to: c.sym,
                from_at: from.at,
                to_at: c.at,
                provider: provider.clone(),
                resolution,
                label: label.to_string(),
                assumptions,
                candidates: n,
                contract,
            });
        }
    }

    pub fn diag(&mut self, kind: &str, file: Option<String>, message: String) {
        if self.diagnostics.len() < 200 {
            self.diagnostics.push(Diagnostic::new(kind, file, message));
        }
    }

    // ---- module resolution (syntax facts only) --------------------------------------------

    /// Python source file of a dotted module (absolute from the root, else a unique suffix
    /// match for `src/` layouts; relative `..x` from `importer`).
    pub fn python_module_file(&self, importer: FileId, module: &str) -> Option<FileId> {
        let dots = module.len() - module.trim_start_matches('.').len();
        let rest = module.trim_start_matches('.');
        if dots > 0 {
            let mut dir: Vec<&str> = parent_dir(self.path(importer))
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            for _ in 1..dots {
                dir.pop()?;
            }
            let base = dir.join(".");
            let full = match (base.is_empty(), rest.is_empty()) {
                (true, _) => rest.to_string(),
                (false, true) => base,
                (false, false) => format!("{base}.{rest}"),
            };
            return self.py_full.get(&full).copied();
        }
        if let Some(&f) = self.py_full.get(rest) {
            return Some(f);
        }
        match self.py_modules.get(rest) {
            Some(v) if v.len() == 1 => Some(v[0]),
            _ => None,
        }
    }

    /// Resolve a Python import target (`app.routes.users.router`) to the longest module
    /// prefix that is a source file, plus the remaining attribute path.
    pub fn resolve_python_target(&self, importer: FileId, target: &str) -> Option<(FileId, Vec<String>)> {
        let dots = target.len() - target.trim_start_matches('.').len();
        let segs: Vec<&str> = target
            .trim_start_matches('.')
            .split('.')
            .filter(|s| !s.is_empty())
            .collect();
        for k in (0..=segs.len()).rev() {
            let module = format!("{}{}", ".".repeat(dots), segs[..k].join("."));
            if module.is_empty() {
                continue;
            }
            if let Some(f) = self.python_module_file(importer, &module) {
                return Some((f, segs[k..].iter().map(|s| s.to_string()).collect()));
            }
        }
        None
    }

    /// Resolve a relative JS/TS module specifier to an indexed file.
    pub fn resolve_js_specifier(&self, importer: FileId, spec: &str) -> Option<FileId> {
        if !(spec.starts_with("./") || spec.starts_with("../")) {
            return None;
        }
        let joined = join_rel(parent_dir(self.path(importer)), spec)?;
        const EXTS: [&str; 8] = ["", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts"];
        for ext in EXTS {
            if let Some(f) = self.index.file_by_path(&format!("{joined}{ext}")) {
                return Some(f);
            }
        }
        for ext in &EXTS[1..] {
            if let Some(f) = self.index.file_by_path(&format!("{joined}/index{ext}")) {
                return Some(f);
            }
        }
        None
    }

    /// Resolve `name` (optionally dotted) used in `file` through that file's imports to
    /// `(file, rest)`: Python module targets and JS/TS relative specifiers.
    pub fn resolve_import(&self, file: FileId, dotted: &str) -> Option<(FileId, Vec<String>)> {
        let facts = self.file_facts(file)?;
        let mut segs = dotted.split('.');
        let root = segs.next()?;
        let rest: Vec<String> = segs.map(str::to_string).collect();
        let import = facts.imports.iter().find(|i| i.local == root)?;
        match self.language(file) {
            Language::Python => {
                let (f, mut attr) = self.resolve_python_target(file, &import.target)?;
                attr.extend(rest);
                Some((f, attr))
            }
            Language::JavaScript | Language::TypeScript | Language::Tsx => {
                let (spec, export) = split_js_target(&import.target, import.kind);
                let f = self.resolve_js_specifier(file, spec)?;
                let mut attr = Vec::new();
                if let Some(e) = export {
                    attr.push(e.to_string());
                }
                attr.extend(rest);
                Some((f, attr))
            }
            _ => None,
        }
    }
}

/// Clamp a tier to the strongest the kind may carry (`Tier` orders proven < possible).
pub(crate) fn clamp(kind: BridgeKind, tier: Tier) -> Tier {
    tier.max(kind.max_tier())
}

pub(crate) fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

pub(crate) fn file_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.split('.').next().unwrap_or(name)
}

/// Join a relative path to a directory, resolving `.` and `..` (never above the root).
pub(crate) fn join_rel(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Dotted module of a Python source path (`pkg/sub/__init__.py` -> `pkg.sub`).
pub(crate) fn python_module_of_path(path: &str) -> String {
    let no_ext = path
        .strip_suffix(".py")
        .or_else(|| path.strip_suffix(".pyi"))
        .unwrap_or(path);
    let no_init = no_ext.strip_suffix("/__init__").unwrap_or(no_ext);
    if no_init == "__init__" {
        return String::new();
    }
    no_init.replace('/', ".")
}

/// `Import::target` of JS/TS -> (specifier, export name). Namespace imports bind the module.
pub(crate) fn split_js_target(target: &str, kind: trace_core::facts::ImportKind) -> (&str, Option<&str>) {
    match kind {
        trace_core::facts::ImportKind::Member => match target.rsplit_once('.') {
            Some((spec, export)) if !spec.is_empty() => (spec, Some(export)),
            _ => (target, None),
        },
        _ => (target, None),
    }
}

/// Lower-case the first character (gRPC method spelling differs per language:
/// `SayHello` in the proto / Go / C# / Python, `sayHello` in Java / Node).
pub(crate) fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Split a normalized path (`/users/{}`) into segments. Code-side keys are normalized by
/// `http::normalize` (placeholders are `{}`); contract paths keep OpenAPI's `{name}`
/// template syntax, which is a placeholder too.
pub(crate) fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('{') && s.ends_with('}') {
                "{}".to_string()
            } else {
                s.to_string()
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/unit/ctx.rs"]
mod tests;
