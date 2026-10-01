//! ABI naming rules: C symbol names (`c_abi`, `c_cpp`), JNI names (`jni`), cgo (`cgo`) and
//! FFI lookups (`ffi`, possible only).
//!
//! C symbol names are global in a linked program, so a declaration links to *the*
//! definition with its name. The rule is proven only when exactly one definition in the
//! repository carries the name (otherwise one possible row per candidate): build
//! configurations that select between same-named definitions are not modelled.

use std::collections::HashMap;

use trace_core::facts::BoundaryRole;
use trace_core::model::{BridgeKind, Provider, Resolution, Tier};
use trace_core::Language;

use crate::ctx::{Ctx, End, Fact};

fn is_c_family(l: Language) -> bool {
    trace_core::languages::info(l).family == Some(trace_core::languages::Family::C)
}

/// Definitions providing C symbols, keyed by name.
fn c_definitions<'a>(ctx: &Ctx<'a>) -> HashMap<&'a str, Vec<Fact<'a>>> {
    let mut map: HashMap<&str, Vec<Fact<'a>>> = HashMap::new();
    for f in ctx.of(BridgeKind::CAbi, BoundaryRole::Provides) {
        map.entry(f.name()).or_default().push(f);
    }
    map
}

pub(crate) fn detect(ctx: &mut Ctx<'_>, weak: bool) {
    let defs = c_definitions(ctx);
    let uses = ctx.of(BridgeKind::CAbi, BoundaryRole::Uses);
    for u in &uses {
        let Some(from) = ctx.end_of(u) else { continue };
        let all = defs.get(u.name()).map(Vec::as_slice).unwrap_or(&[]);
        if u.language == Language::Rust {
            // Rust `extern "C" { fn f(); }` -> C/C++ definition.
            let cands: Vec<End> = all
                .iter()
                .filter(|d| is_c_family(d.language))
                .filter_map(|d| ctx.end_of(d))
                .collect();
            if !cands.is_empty() {
                ctx.emit(
                    BridgeKind::CAbi,
                    from,
                    &cands,
                    Tier::Proven,
                    Provider::Rule("c-abi".into()),
                    Resolution::AbiNamingRule,
                    &format!("c:{}", u.name()),
                    &["C symbol names are global: the Rust extern declaration links to the C definition with this name".into()],
                    None,
                );
            }
            continue;
        }
        if !is_c_family(u.language) {
            continue;
        }
        // C/C++ prototype -> Go function exported to C with cgo `//export name` (cgo
        // generates the C entry point with exactly that name).
        let go: Vec<End> = all
            .iter()
            .filter(|d| d.language == Language::Go)
            .filter_map(|d| ctx.end_of(d))
            .collect();
        let rust_exports = all.iter().any(|d| d.language == Language::Rust);
        if !go.is_empty() && !rust_exports {
            let mut cands = go;
            cands.extend(
                all.iter()
                    .filter(|d| is_c_family(d.language) && d.file != u.file)
                    .filter_map(|d| ctx.end_of(d)),
            );
            ctx.emit(
                BridgeKind::Cgo,
                from,
                &cands,
                Tier::Proven,
                Provider::Rule("cgo-export".into()),
                Resolution::AbiNamingRule,
                &format!("c:{}", u.name()),
                &["cgo //export: the C declaration links to the Go function exported under this name (cgo generates its C entry point)".into()],
                None,
            );
            continue;
        }
        // C/C++ prototype -> Rust `#[no_mangle]` export.
        let rust: Vec<End> = all
            .iter()
            .filter(|d| d.language == Language::Rust)
            .filter_map(|d| ctx.end_of(d))
            .collect();
        if !rust.is_empty() {
            let mut assumptions = vec![
                "C symbol names are global: the C declaration links to the Rust export with this name"
                    .to_string(),
            ];
            if all
                .iter()
                .any(|d| d.language == Language::Rust && d.detail("abi") == Some("rust"))
            {
                assumptions
                    .push("the Rust function is exported unmangled but not declared extern \"C\"".into());
            }
            // Same-named C/C++ definitions compete with the Rust export.
            let mut cands = rust;
            cands.extend(
                all.iter()
                    .filter(|d| is_c_family(d.language) && d.file != u.file)
                    .filter_map(|d| ctx.end_of(d)),
            );
            ctx.emit(
                BridgeKind::CAbi,
                from,
                &cands,
                Tier::Proven,
                Provider::Rule("c-abi".into()),
                Resolution::AbiNamingRule,
                &format!("c:{}", u.name()),
                &assumptions,
                None,
            );
            continue;
        }
        // C <-> C++ across `extern "C"`.
        let linkage = u.detail("linkage").unwrap_or("c");
        let other: Vec<End> = all
            .iter()
            .filter(|d| {
                let dl = d.detail("linkage").unwrap_or("c");
                (u.language == Language::C
                    && linkage == "c"
                    && d.language == Language::Cpp
                    && dl == "cpp_extern_c")
                    || (u.language == Language::Cpp && linkage == "cpp_extern_c" && d.language == Language::C)
            })
            .filter_map(|d| ctx.end_of(d))
            .collect();
        if !other.is_empty() {
            // Same-language definitions with the same name are competing candidates.
            let mut cands = other;
            cands.extend(
                all.iter()
                    .filter(|d| d.language == u.language && d.file != u.file)
                    .filter_map(|d| ctx.end_of(d)),
            );
            ctx.emit(
                BridgeKind::CCpp,
                from,
                &cands,
                Tier::Proven,
                Provider::Rule("c-linkage".into()),
                Resolution::AbiNamingRule,
                &format!("c:{}", u.name()),
                &[
                    "C linkage: the declaration and the extern \"C\" definition share one unmangled name"
                        .into(),
                ],
                None,
            );
        }
    }
    jni(ctx);
    cgo(ctx, &defs);
    if weak {
        ffi(ctx, &defs);
    }
}

fn jni(ctx: &mut Ctx<'_>) {
    // Providers keyed by their short name and by every `__` prefix (overload suffixes).
    let mut by_key: HashMap<String, Vec<Fact<'_>>> = HashMap::new();
    for p in ctx.of(BridgeKind::Jni, BoundaryRole::Provides) {
        let name = p.name();
        by_key.entry(name.to_string()).or_default().push(p);
        let mut search = 0;
        while let Some(i) = name[search..].find("__") {
            let at = search + i;
            if at > 5 {
                by_key.entry(name[..at].to_string()).or_default().push(p);
            }
            search = at + 2;
        }
    }
    for u in ctx.of(BridgeKind::Jni, BoundaryRole::Uses) {
        let Some(from) = ctx.end_of(&u) else { continue };
        let Some(ps) = by_key.get(u.name()) else { continue };
        let cands: Vec<End> = ps.iter().filter_map(|p| ctx.end_of(p)).collect();
        ctx.emit(
            BridgeKind::Jni,
            from,
            &cands,
            Tier::Proven,
            Provider::Rule("jni-naming".into()),
            Resolution::AbiNamingRule,
            u.name(),
            &["JNI naming rule: Java_<mangled class>_<mangled method> (static registration; RegisterNatives tables are not modelled)".into()],
            None,
        );
    }
}

fn cgo(ctx: &mut Ctx<'_>, defs: &HashMap<&str, Vec<Fact<'_>>>) {
    let preamble = ctx.of(BridgeKind::Cgo, BoundaryRole::Provides);
    let prototypes = ctx.of(BridgeKind::CAbi, BoundaryRole::Uses);
    for u in ctx.of(BridgeKind::Cgo, BoundaryRole::Uses) {
        let Some(from) = ctx.end_of(&u) else { continue };
        let name = u.name();
        // 1. Defined in this file's preamble; 2. a C/C++ definition in the repository;
        // 3. declared in the preamble or in an included header only.
        let own: Vec<&Fact<'_>> = preamble
            .iter()
            .filter(|p| p.file == u.file && p.name() == name)
            .collect();
        let own_defs: Vec<End> = own
            .iter()
            .filter(|p| p.flag("definition"))
            .filter_map(|p| ctx.end_of(p))
            .collect();
        let (cands, assumption) = if !own_defs.is_empty() {
            (own_defs, "defined in the cgo preamble of this file")
        } else {
            let repo_defs: Vec<End> = defs
                .get(name)
                .map(|v| {
                    v.iter()
                        .filter(|d| is_c_family(d.language))
                        .filter_map(|d| ctx.end_of(d))
                        .collect()
                })
                .unwrap_or_default();
            if !repo_defs.is_empty() {
                (repo_defs, "C symbol names are global: C.name links to the C definition with this name")
            } else {
                let includes: Vec<&str> = u
                    .detail("includes")
                    .map(|s| s.split(',').collect())
                    .unwrap_or_default();
                let mut decls: Vec<End> = own.iter().filter_map(|p| ctx.end_of(p)).collect();
                let visible: Vec<_> = prototypes
                    .iter()
                    .filter(|p| p.name() == name && is_c_family(p.language))
                    .filter(|p| {
                        let path = ctx.path(p.file);
                        let base = path.rsplit('/').next().unwrap_or(path);
                        includes.iter().any(|i| i.rsplit('/').next() == Some(base))
                    })
                    .collect();
                decls.extend(visible.into_iter().filter_map(|p| ctx.end_of(p)));
                (decls, "only a declaration is visible (included header / preamble); the definition is outside the repository")
            }
        };
        if cands.is_empty() {
            continue;
        }
        ctx.emit(
            BridgeKind::Cgo,
            from,
            &cands,
            Tier::Proven,
            Provider::Rule("cgo".into()),
            Resolution::AbiNamingRule,
            &format!("C.{name}"),
            &[assumption.to_string()],
            None,
        );
    }
}

fn ffi(ctx: &mut Ctx<'_>, defs: &HashMap<&str, Vec<Fact<'_>>>) {
    for u in ctx.of(BridgeKind::Ffi, BoundaryRole::Uses) {
        let Some(from) = ctx.end_of(&u) else { continue };
        let cands: Vec<End> = defs
            .get(u.name())
            .map(|v| {
                v.iter()
                    .filter(|d| d.file != u.file)
                    .filter_map(|d| ctx.end_of(d))
                    .collect()
            })
            .unwrap_or_default();
        if cands.is_empty() {
            continue;
        }
        let loader = u.detail("loader").unwrap_or("dynamic loader");
        ctx.emit(
            BridgeKind::Ffi,
            from,
            &cands,
            Tier::Possible,
            Provider::Rule("ffi".into()),
            Resolution::AbiNamingRule,
            &format!("c:{}", u.name()),
            &[format!(
                "symbol looked up at runtime ({loader}); the loaded library path is not verified"
            )],
            None,
        );
    }
}
