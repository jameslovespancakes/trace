//! Binding attributes and export tables (`pyo3`, `cpython`, `wasm_bindgen`, `napi`) and the
//! packaging rules of item 22 (`python_stub` for compiled-extension stubs, `js_ts` for
//! `.d.ts` declarations of JavaScript modules).
//!
//! A binding attribute defines the name the other language sees; the rule is proven when
//! the used name matches exactly one exported declaration (and the module identity is
//! established: a registered / unique extension module, a wasm-pack package name),
//! otherwise every candidate is possible.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use trace_core::facts::{BoundaryRole, ImportKind};
use trace_core::languages::{in_family, Family};
use trace_core::model::{BridgeKind, FileId, Provider, Resolution, SymbolId, SymbolKind, Tier};
use trace_core::{Hash32, Language};

use crate::ctx::{file_stem, parent_dir, split_js_target, Ctx, End};

/// A Python-visible export of a binding.
#[derive(Clone, Debug)]
struct Export {
    kind: BridgeKind,
    /// Extension module name (`_core`), `None` when it cannot be established.
    module: Option<String>,
    key: String,
    end: End,
    constructor: bool,
    class: bool,
}

fn python_exports(ctx: &mut Ctx<'_>) -> Vec<Export> {
    let pyo3 = ctx.of(BridgeKind::Pyo3, BoundaryRole::Provides);
    // Modules and what they register (Rust names).
    let modules: Vec<(String, HashSet<String>)> = pyo3
        .iter()
        .filter(|f| f.flag("module_init"))
        .map(|f| {
            let regs = f
                .detail("registers")
                .map(|r| r.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect())
                .unwrap_or_default();
            (f.detail("module").unwrap_or(f.name()).to_string(), regs)
        })
        .collect();
    let module_of = |rust_name: &str| -> Option<String> {
        let registering: Vec<&String> = modules
            .iter()
            .filter(|(_, r)| r.contains(rust_name))
            .map(|(m, _)| m)
            .collect();
        match (registering.len(), modules.len()) {
            (1, _) => Some(registering[0].clone()),
            (0, 1) => Some(modules[0].0.clone()),
            _ => None,
        }
    };
    // Rust type -> Python class name(s).
    let mut classes: HashMap<String, Vec<String>> = HashMap::new();
    for f in pyo3.iter().filter(|f| f.detail("item") == Some("class")) {
        if let Some(rust) = f.detail("rust_name") {
            classes
                .entry(rust.to_string())
                .or_default()
                .push(f.name().to_string());
        }
    }
    let mut out = Vec::new();
    for f in &pyo3 {
        if f.flag("module_init") {
            continue;
        }
        let Some(end) = ctx.end_of(f) else { continue };
        match f.detail("item") {
            Some("method") => {
                let ty = f.detail("impl_type").unwrap_or_default();
                let method = f.name().rsplit_once('.').map(|(_, m)| m).unwrap_or(f.name());
                let py_classes = classes.get(ty).cloned().unwrap_or_else(|| vec![ty.to_string()]);
                let module = module_of(ty);
                let constructor = f.flag("constructor");
                for class in py_classes {
                    out.push(Export {
                        kind: BridgeKind::Pyo3,
                        module: module.clone(),
                        key: format!("{class}.{method}"),
                        end,
                        constructor,
                        class: false,
                    });
                    if constructor {
                        out.push(Export {
                            kind: BridgeKind::Pyo3,
                            module: module.clone(),
                            key: class.clone(),
                            end,
                            constructor: true,
                            class: false,
                        });
                    }
                }
            }
            item => {
                let rust = f.detail("rust_name").unwrap_or(f.name());
                out.push(Export {
                    kind: BridgeKind::Pyo3,
                    module: f.detail("module").map(str::to_string).or_else(|| module_of(rust)),
                    key: f.name().to_string(),
                    end,
                    constructor: false,
                    class: item == Some("class"),
                });
            }
        }
    }
    for f in ctx.of(BridgeKind::Cpython, BoundaryRole::Provides) {
        let Some(end) = ctx.end_of(&f) else { continue };
        out.push(Export {
            kind: BridgeKind::Cpython,
            module: f.detail("module").map(str::to_string),
            key: f.name().to_string(),
            end,
            constructor: false,
            class: false,
        });
    }
    out
}

fn candidates<'e>(exports: &'e [Export], module: &str, key: &str, want_class: bool) -> Vec<&'e Export> {
    let mut c: Vec<&Export> = exports
        .iter()
        .filter(|e| e.key == key && e.module.as_deref().is_none_or(|m| m == module))
        .collect();
    if want_class {
        let classes: Vec<&Export> = c.iter().copied().filter(|e| e.class).collect();
        if !classes.is_empty() {
            c = classes;
        }
    } else if c.iter().any(|e| e.constructor) {
        // A class call runs its `#[new]` constructor.
        c.retain(|e| e.constructor);
    }
    c
}

pub(crate) fn detect(ctx: &mut Ctx<'_>) {
    let exports = python_exports(ctx);
    if !exports.is_empty() {
        python_uses(ctx, &exports);
        python_stubs(ctx, &exports);
    }
    js_bindings(ctx);
    dts_rule(ctx);
}

/// Python calls into extension modules without Python source, from import-resolved callee
/// paths of calls the semantic backend left unresolved (or of syntax-only files).
fn python_uses(ctx: &mut Ctx<'_>, exports: &[Export]) {
    let modules: HashSet<&str> = exports.iter().filter_map(|e| e.module.as_deref()).collect();
    if modules.is_empty() {
        return;
    }
    let index = ctx.index;
    for (fi, rec) in index.files.iter().enumerate() {
        let file = FileId(fi as u32);
        if rec.language != Language::Python || Language::is_python_stub_path(&rec.path) {
            continue;
        }
        let Some(facts) = &rec.facts else { continue };
        let semantic = rec.semantic.is_some();
        for (ci, call) in facts.calls.iter().enumerate() {
            let Some(path) = facts.call_detail(ci).and_then(|d| d.callee_path.as_deref()) else { continue };
            if semantic
                && !ctx
                    .unresolved_spans
                    .contains(&(file, call.callee_span.start, call.callee_span.end))
            {
                continue;
            }
            let segs: Vec<&str> = path
                .trim_start_matches('.')
                .split('.')
                .filter(|s| !s.is_empty())
                .collect();
            // Longest module prefix whose last segment is an extension module name and that
            // is not a Python source module.
            let Some(k) = (1..segs.len()).rev().find(|&k| {
                modules.contains(segs[k - 1]) && ctx.python_module_file(file, &segs[..k].join(".")).is_none()
            }) else {
                continue;
            };
            let module = segs[k - 1];
            let key = segs[k..].join(".");
            let cands = candidates(exports, module, &key, false);
            if cands.is_empty() {
                continue;
            }
            let Some(sym) = ctx.executing(file, call.owner, call.span.start) else { continue };
            let from = End {
                sym,
                at: trace_core::model::Location {
                    file,
                    bytes: call.callee_span,
                    line: call.line,
                },
            };
            let proven = cands.iter().all(|e| e.module.is_some());
            let kind = cands[0].kind;
            let ends: Vec<End> = cands.iter().map(|e| e.end).collect();
            let (provider, assumption) = match kind {
                BridgeKind::Cpython => {
                    ("cpython-methoddef", "PyMethodDef table entry defines the Python-visible name")
                }
                _ => ("pyo3", "PyO3 binding attribute defines the Python-visible name"),
            };
            let mut assumptions = vec![format!(
                "{assumption}; `{module}` is the compiled extension module (no Python source)"
            )];
            if !proven {
                assumptions.push("the extension module of the export could not be established".into());
            }
            ctx.emit(
                kind,
                from,
                &ends,
                if proven { Tier::Proven } else { Tier::Possible },
                Provider::Rule(provider.into()),
                Resolution::BindingAttribute,
                &format!("{module}.{key}"),
                &assumptions,
                None,
            );
        }
    }
}

/// `.pyi` stubs of compiled modules (no `.py` sibling, no package `__init__.py`) -> the
/// binding declaration exporting the same name.
fn python_stubs(ctx: &mut Ctx<'_>, exports: &[Export]) {
    let index = ctx.index;
    for (fi, rec) in index.files.iter().enumerate() {
        if rec.language != Language::Python || !Language::is_python_stub_path(&rec.path) {
            continue;
        }
        let file = FileId(fi as u32);
        let stem = file_stem(&rec.path).to_string();
        let dir = parent_dir(&rec.path);
        let sibling = |name: &str| {
            let p = if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            };
            index.file_by_path(&p).is_some()
        };
        if sibling(&format!("{stem}.py")) || sibling(&format!("{stem}/__init__.py")) {
            continue; // Existing stub rule (`.pyi` beside `.py`) applies.
        }
        let module = if stem == "__init__" {
            dir.rsplit('/').next().unwrap_or("").to_string()
        } else {
            stem.clone()
        };
        for s in index.symbols_of(file) {
            if s.is_synthetic() || s.kind == SymbolKind::Module {
                continue;
            }
            let (key, want_class) = match s.name.as_str() {
                "__init__" | "__new__" => match s.qualified_name.rsplit_once('.') {
                    Some((class, _)) => (format!("{class}.__new__"), false),
                    None => continue,
                },
                _ => (s.qualified_name.clone(), s.kind.is_type()),
            };
            let cands = candidates(exports, &module, &key, want_class);
            if cands.is_empty() {
                continue;
            }
            let proven = cands.iter().all(|e| e.module.as_deref() == Some(module.as_str()));
            let ends: Vec<End> = cands.iter().map(|e| e.end).collect();
            let from = ctx.symbol_end(s.id);
            ctx.emit(
                BridgeKind::PythonStub,
                from,
                &ends,
                if proven { Tier::Proven } else { Tier::Possible },
                Provider::Rule("python-stub-binding".into()),
                Resolution::StubPackagingRule,
                &format!("{module}.{key}"),
                &[format!(
                    "`{}` is the type stub of the compiled module `{module}` (no Python source); the binding exports `{key}`",
                    rec.path
                )],
                None,
            );
        }
    }
}

/// Crate names of Rust crates in the repository (`[package] name` of indexed
/// `Cargo.toml` configuration files, read verified; normalized `-` -> `_`), by crate dir.
fn crate_names(ctx: &Ctx<'_>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (path, hash) in &ctx.index.configs {
        if !(path == "Cargo.toml" || path.ends_with("/Cargo.toml")) {
            continue;
        }
        let full = ctx.sources.root().join(Path::new(path));
        let Ok(bytes) = std::fs::read(&full) else { continue };
        if Hash32::of(&bytes) != *hash || bytes.len() > 1_000_000 {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        if let Some(name) = cargo_package_name(&text) {
            out.push((parent_dir(path).to_string(), name.replace('-', "_")));
        }
    }
    out
}

/// `[package] name = "x"` of a Cargo manifest (line-structured TOML table reader).
pub(crate) fn cargo_package_name(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if key.trim() == "name" {
            let v = value.trim().trim_matches('"').trim_matches('\'');
            return (!v.is_empty()).then(|| v.to_string());
        }
    }
    None
}

/// JS/TS uses of wasm-bindgen / N-API exports: calls through imports of the generated
/// package (`import { Universe } from "wasm-game-of-life"` + `Universe.new()`), imports of
/// `.node` / `.wasm` modules, and `require`-loaded addons (boundary facts).
fn js_bindings(ctx: &mut Ctx<'_>) {
    let mut exports: HashMap<String, Vec<(BridgeKind, End, FileId)>> = HashMap::new();
    // Export keys `Class.f` whose Rust function returns the exported class itself.
    let mut returns_self: HashSet<String> = HashSet::new();
    for kind in [BridgeKind::WasmBindgen, BridgeKind::Napi] {
        for f in ctx.of(kind, BoundaryRole::Provides) {
            if let Some(end) = ctx.end_of(&f) {
                exports
                    .entry(f.name().to_string())
                    .or_default()
                    .push((kind, end, f.file));
                if f.flag("returns_self") {
                    returns_self.insert(f.name().to_string());
                }
            }
        }
    }
    if exports.is_empty() {
        return;
    }
    let crates = crate_names(ctx);
    let crate_dirs_of_exports: Vec<&str> = exports
        .values()
        .flatten()
        .map(|(_, _, f)| ctx.path(*f))
        .filter_map(|p| {
            crates
                .iter()
                .filter(|(dir, _)| dir.is_empty() || p.starts_with(&format!("{dir}/")))
                .max_by_key(|(dir, _)| dir.len())
                .map(|(_, n)| n.as_str())
        })
        .collect();
    let binding_package = |spec: &str| -> Option<bool> {
        // Some(true) = established package identity, Some(false) = plausible only.
        let lower = spec.to_ascii_lowercase();
        if lower.ends_with(".wasm") || lower.ends_with(".node") {
            return Some(true);
        }
        let segs: Vec<String> = spec
            .split('/')
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .map(|s| {
                let s = s.trim_start_matches('@');
                let s = s.split('.').next().unwrap_or(s);
                s.trim_end_matches("_bg").replace('-', "_")
            })
            .collect();
        if segs
            .iter()
            .any(|s| crate_dirs_of_exports.contains(&s.as_str()) || s == "pkg")
        {
            return Some(true);
        }
        None
    };
    let index = ctx.index;
    for (fi, rec) in index.files.iter().enumerate() {
        if !in_family(rec.language, Family::JavaScript) {
            continue;
        }
        let file = FileId(fi as u32);
        let Some(facts) = &rec.facts else { continue };
        // Local name -> (specifier, export name, package identity established).
        let mut locals: HashMap<&str, (&str, Option<&str>, bool)> = HashMap::new();
        for imp in &facts.imports {
            let (spec, export) = split_js_target(&imp.target, imp.kind);
            let relative = spec.starts_with("./") || spec.starts_with("../");
            let established = match binding_package(spec) {
                Some(e) => e,
                // A relative specifier that resolves to no indexed file (generated output).
                None if relative && ctx.resolve_js_specifier(file, spec).is_none() => false,
                None => continue,
            };
            locals.insert(imp.local.as_str(), (spec, export.filter(|e| *e != "default"), established));
        }
        if locals.is_empty() {
            continue;
        }
        let instances = js_instances(facts, &locals, &returns_self);
        for call in &facts.calls {
            let root = call
                .callee
                .split(['.', '(', '<', '?', '!'])
                .next()
                .unwrap_or("")
                .trim();
            let root = root.strip_prefix("new ").unwrap_or(root).trim();
            let rest: Vec<&str> = call.callee.split('.').skip(1).map(str::trim).collect();
            let (key, established) = if let Some((_, export, established)) = locals.get(root) {
                let key = match (export, rest.is_empty()) {
                    (Some(e), true) => e.to_string(),
                    (Some(e), false) => format!("{e}.{}", rest.join(".")),
                    (None, false) => rest.join("."),
                    (None, true) => continue,
                };
                (key, *established)
            } else if let Some((class, established)) = instances.get(root) {
                // A method call on an instance of an exported class (`universe.tick()`).
                if rest.len() != 1 || call.is_new {
                    continue;
                }
                let Some(sym) = ctx.executing(file, call.owner, call.span.start) else { continue };
                if shadows(ctx, sym, root) {
                    continue;
                }
                (format!("{class}.{}", rest[0]), *established)
            } else {
                continue;
            };
            let Some(cands) = exports.get(&key) else { continue };
            let Some(sym) = ctx.executing(file, call.owner, call.span.start) else { continue };
            let from = End {
                sym,
                at: trace_core::model::Location {
                    file,
                    bytes: call.callee_span,
                    line: call.line,
                },
            };
            emit_js(ctx, from, cands, &key, established);
        }
        // Property reads of exported enums / classes (`Cell.Alive`): the object property the
        // binding defines. The reference is the property name; the text before it must be
        // exactly `<imported local>.`.
        for r in &facts.references {
            if r.kind != trace_core::facts::RefKind::Read {
                continue;
            }
            for (local, (_, export, established)) in &locals {
                let Some(export) = export else { continue };
                let key = format!("{export}.{}", r.name);
                let Some(cands) = exports.get(&key) else { continue };
                let n = local.len() as u32 + 1;
                if r.span.start < n {
                    continue;
                }
                let before = trace_core::ByteSpan::new(r.span.start - n, r.span.start);
                if ctx.sources.text(file, before).ok().as_deref() != Some(&format!("{local}.")) {
                    continue;
                }
                let Some(sym) = ctx.executing(file, r.owner, r.span.start) else { continue };
                let line = ctx
                    .sources
                    .line_at(file, r.span.start)
                    .map(|(l, _, _)| l)
                    .unwrap_or(0);
                let from = End {
                    sym,
                    at: trace_core::model::Location {
                        file,
                        bytes: trace_core::ByteSpan::new(before.start, r.span.end),
                        line,
                    },
                };
                emit_js(ctx, from, cands, &key, *established);
            }
        }
        // Imported names: the import binds them (instances, re-exports, and classes whose
        // static functions are called as well).
        for imp in &facts.imports {
            let Some((_, Some(export), established)) = locals.get(imp.local.as_str()) else { continue };
            if imp.kind != ImportKind::Member {
                continue;
            }
            let Some(cands) = exports.get(*export) else { continue };
            let Some(sym) = ctx.executing(file, None, imp.span.start) else { continue };
            let from = End {
                sym,
                at: trace_core::model::Location {
                    file,
                    bytes: imp.span,
                    line: imp.line,
                },
            };
            emit_js(ctx, from, cands, export, *established);
        }
    }
    // `require`-loaded N-API addons (boundary facts from trace-syntax).
    for u in ctx.of(BridgeKind::Napi, BoundaryRole::Uses) {
        let Some(from) = ctx.end_of(&u) else { continue };
        let Some(cands) = exports.get(u.name()) else { continue };
        let cands: Vec<(BridgeKind, End, FileId)> = cands
            .iter()
            .filter(|(k, _, _)| *k == BridgeKind::Napi)
            .cloned()
            .collect();
        if cands.is_empty() {
            continue;
        }
        emit_js(ctx, from, &cands, u.name(), true);
    }
}

/// Variables bound to instances of an exported class: the file's only binding of the name is
/// `x = Class.f(..)` where `Class` is an imported binding export and the Rust function `f`
/// returns the class itself (`const universe = Universe.new()`). Any second binding of the
/// name anywhere in the file drops it. Value: (class export name, package established).
fn js_instances<'f>(
    facts: &'f trace_core::facts::FileFacts,
    locals: &HashMap<&str, (&str, Option<&str>, bool)>,
    returns_self: &HashSet<String>,
) -> HashMap<&'f str, (String, bool)> {
    let mut bindings: HashMap<&str, usize> = HashMap::new();
    for a in &facts.assignments {
        if a.kind == trace_core::facts::AssignmentKind::Name {
            *bindings.entry(a.target.as_str()).or_default() += 1;
        }
    }
    let mut out: HashMap<&'f str, (String, bool)> = HashMap::new();
    for a in &facts.assignments {
        if a.kind != trace_core::facts::AssignmentKind::Name || bindings.get(a.target.as_str()) != Some(&1) {
            continue;
        }
        // The outermost call inside the binding statement is its value.
        let Some(call) = facts
            .calls
            .iter()
            .filter(|c| a.span.start <= c.span.start && c.span.end <= a.span.end)
            .max_by_key(|c| c.span.len())
        else {
            continue;
        };
        if call.span.end != a.span.end {
            continue;
        }
        let root = call.callee.split('.').next().unwrap_or("").trim();
        let Some((_, Some(export), established)) = locals.get(root) else { continue };
        let rest: Vec<&str> = call.callee.split('.').skip(1).map(str::trim).collect();
        if rest.len() != 1 || call.is_new {
            continue;
        }
        if returns_self.contains(&format!("{export}.{}", rest[0])) {
            out.insert(a.target.as_str(), (export.to_string(), *established));
        }
    }
    out
}

/// A parameter of the executing function or an enclosing one rebinds `name`.
fn shadows(ctx: &Ctx<'_>, sym: SymbolId, name: &str) -> bool {
    let mut cur = Some(sym);
    let mut depth = 0;
    while let Some(s) = cur {
        let s = ctx.index.symbol(s);
        if s.parameters.iter().any(|p| p == name) {
            return true;
        }
        cur = s.parent;
        depth += 1;
        if depth > 32 {
            break;
        }
    }
    false
}

fn emit_js(ctx: &mut Ctx<'_>, from: End, cands: &[(BridgeKind, End, FileId)], key: &str, established: bool) {
    let kind = cands[0].0;
    let ends: Vec<End> = cands
        .iter()
        .filter(|(k, _, _)| *k == kind)
        .map(|(_, e, _)| *e)
        .collect();
    let (provider, what) = match kind {
        BridgeKind::Napi => ("napi", "N-API export"),
        _ => ("wasm-bindgen", "#[wasm_bindgen] export"),
    };
    let mut assumptions = vec![format!("{what} defines the JavaScript-visible name `{key}`")];
    if !established {
        assumptions.push(
            "the imported module is assumed to be the generated binding package (it is not in the index)"
                .into(),
        );
    }
    ctx.emit(
        kind,
        from,
        &ends,
        if established { Tier::Proven } else { Tier::Possible },
        Provider::Rule(provider.into()),
        Resolution::BindingAttribute,
        key,
        &assumptions,
        None,
    );
}

/// `.d.ts` declaration files -> sibling JavaScript implementation with the same export
/// (TypeScript module resolution of `x.d.ts` next to `x.js`).
fn dts_rule(ctx: &mut Ctx<'_>) {
    let index = ctx.index;
    for (fi, rec) in index.files.iter().enumerate() {
        let lower = rec.path.to_ascii_lowercase();
        let (stem, impl_exts): (&str, &[&str]) = if let Some(s) = lower.strip_suffix(".d.ts") {
            (&rec.path[..s.len()], &[".js", ".jsx", ".cjs", ".mjs"])
        } else if let Some(s) = lower.strip_suffix(".d.mts") {
            (&rec.path[..s.len()], &[".mjs"])
        } else if let Some(s) = lower.strip_suffix(".d.cts") {
            (&rec.path[..s.len()], &[".cjs"])
        } else {
            continue;
        };
        let file = FileId(fi as u32);
        let Some(target) = impl_exts
            .iter()
            .find_map(|e| index.file_by_path(&format!("{stem}{e}")))
        else {
            continue;
        };
        let impls: Vec<(String, SymbolId)> = index
            .symbols_of(target)
            .iter()
            .filter(|s| !s.is_synthetic())
            .map(|s| (s.qualified_name.clone(), s.id))
            .collect();
        for s in index.symbols_of(file) {
            if s.is_synthetic() || s.kind == SymbolKind::Module {
                continue;
            }
            let cands: Vec<End> = impls
                .iter()
                .filter(|(q, _)| *q == s.qualified_name)
                .map(|(_, id)| ctx.symbol_end(*id))
                .collect();
            if cands.is_empty() {
                continue;
            }
            let from = ctx.symbol_end(s.id);
            ctx.emit(
                BridgeKind::JsTs,
                from,
                &cands,
                Tier::Proven,
                Provider::Rule("dts-implementation".into()),
                Resolution::StubPackagingRule,
                &s.qualified_name,
                &[format!(
                    "TypeScript resolves `{}` to the declaration file; the sibling `{}` implements it",
                    rec.path,
                    index.file_path(target)
                )],
                None,
            );
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/bindings.rs"]
mod tests;
