//! Providers of installed plugins (rule "provider of an installed plugin").
//!
//! A by-name injection runtime (a `runtime_dispatch` row with pattern
//! [`INJECT_BY_PARAMETER_NAME`] whose `activated_by` package is installed) also passes the
//! values of providers that installed packages declare: the activating package's own
//! distribution and every installed distribution that requires it unconditionally (its
//! plugins; a requirement under an `extra` marker is optional and makes no plugin). This
//! module reads the Python modules those distributions list in their `RECORD` with syntax
//! trees and collects every module-level function decorated with the provider decorator,
//! under the name it provides (the literal of the row's renaming keyword, else its own name),
//! with the library class of its value when its source shows it: every value it returns (a
//! generator: every value it yields, what the runtime passes) is a construction `Class(..)`
//! of one and the same library class, directly or through a local variable every binding of
//! which is such a construction. Nothing is executed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trace_core::facts::{BindTarget, Expr, FileFacts, FlowFact, Scope};
use trace_core::{ExecutionModel, Language};
use trace_env::EcosystemId;
use trace_syntax::lower::{LITERAL_PREFIX, YIELDED};

use crate::derive::{FsLoader, SourceLoader};
use crate::library_class::{qualified, resolve_base, Files};
use crate::model::ArgSel;
use crate::table::{RowSel, Section};
use crate::Library;

/// `pattern` of `runtime_dispatch` rows whose runtime injects provider values into
/// parameters by name (irreducible table, `why_not_derivable: reflection`).
pub const INJECT_BY_PARAMETER_NAME: &str = "inject_by_parameter_name";

/// Python modules read per distribution at most.
const MAX_MODULES: usize = 2_000;

/// A provider an installed distribution declares.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstalledProvider {
    /// Library-qualified provider function (`pkg.plugin.provider`).
    pub symbol: String,
    /// Library-qualified class of the value it provides, when its source shows one.
    pub value: Option<String>,
}

/// Provider name -> the installed providers of that name (sorted).
pub type Providers = BTreeMap<String, Vec<InstalledProvider>>;

/// Active Python by-name injection rows: (package, decorator, renaming keyword).
type Rows = Vec<(String, String, Option<String>)>;

impl Library {
    fn site_roots(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .filter(|r| r.ecosystem == EcosystemId::Python && r.layout == "site_packages")
            .map(|r| r.path.clone())
            .collect()
    }

    fn injection_rows(&self) -> Rows {
        self.tables()
            .irreducible(Language::Python, Section::RuntimeDispatch)
            .iter()
            .filter(|row| row.pattern.as_deref() == Some(INJECT_BY_PARAMETER_NAME))
            .filter_map(|row| {
                let keyword = match row.key_sel() {
                    Ok(Some(RowSel::Arg(ArgSel::Kw(k) | ArgSel::PosOrKw(_, k)))) => Some(k),
                    _ => None,
                };
                Some((row.activated_by.clone()?, row.symbol.clone()?, keyword))
            })
            .collect()
    }

    /// Installed providers of every Python by-name injection row, by the row's provider
    /// decorator (`symbol`).
    pub fn injected_providers(&self) -> BTreeMap<String, Providers> {
        let roots = self.site_roots();
        let search = self.root_paths(Language::Python);
        let mut out = BTreeMap::new();
        for (package, decorator, keyword) in self.injection_rows() {
            let found =
                installed_providers(&FsLoader, &roots, &search, &package, &decorator, keyword.as_deref());
            if !found.is_empty() {
                out.insert(decorator, found);
            }
        }
        out
    }

    /// Fingerprint of what [`Library::injected_providers`] reads: the rows, the search roots
    /// and every installed distribution (`*.dist-info` name, `RECORD` size and modification
    /// time). Equal fingerprints give equal providers, so an edit reuses them.
    pub fn injected_key(&self) -> String {
        installation_key(&self.injection_rows(), &self.root_paths(Language::Python), &self.site_roots())
    }
}

/// [`Library::injected_key`] over explicit rows, search roots and site-packages roots.
fn installation_key(rows: &Rows, search: &[PathBuf], sites: &[PathBuf]) -> String {
    let mut h = blake3::Hasher::new();
    for (package, decorator, keyword) in rows {
        h.update(format!("{package}\0{decorator}\0{keyword:?}\0").as_bytes());
    }
    for root in search {
        h.update(root.to_string_lossy().as_bytes());
        h.update(&[0]);
    }
    for root in sites {
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        let mut dists: Vec<(String, u64, u128)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "dist-info"))
            .map(|p| {
                let meta = std::fs::metadata(p.join("RECORD")).ok();
                let size = meta.as_ref().map_or(0, |m| m.len());
                let modified = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos());
                (p.to_string_lossy().into_owned(), size, modified)
            })
            .collect();
        dists.sort();
        for (name, size, modified) in dists {
            h.update(format!("{name}\0{size}\0{modified}\0").as_bytes());
        }
    }
    h.finalize().to_hex().to_string()
}

/// Providers the distributions of `package` and of its plugins below the site-packages
/// `roots` declare ([`Library::injected_providers`]); `search`: import roots for classes.
pub fn installed_providers(
    loader: &dyn SourceLoader,
    roots: &[PathBuf],
    search: &[PathBuf],
    package: &str,
    decorator: &str,
    keyword: Option<&str>,
) -> Providers {
    let mut files = Files::new(loader, Language::Python);
    let mut out: Providers = BTreeMap::new();
    let modules: Vec<Vec<PathBuf>> = roots.iter().map(|root| distribution_modules(root, package)).collect();
    files.preload(&modules.concat());
    for modules in modules {
        for module in modules {
            let Some(parsed) = files.get(&module) else { continue };
            let found: Vec<(String, usize)> = parsed
                .facts
                .declarations
                .iter()
                .enumerate()
                .filter(|(i, d)| {
                    d.kind.is_callable()
                        && parsed.facts.module_decl != Some(*i as u32)
                        && d.parent.is_none_or(|p| parsed.facts.module_decl == Some(p))
                        && d.decorators.iter().any(|t| names_decorator(decorator, head(t)))
                })
                .map(|(i, d)| {
                    let name = keyword
                        .and_then(|k| renamed(&parsed.facts, i, decorator, k))
                        .unwrap_or_else(|| d.name.clone());
                    (name, i)
                })
                .collect();
            for (name, function) in found {
                let Some(parsed) = files.get(&module) else { continue };
                let symbol = qualified(Language::Python, &module, &parsed.facts, function);
                let value = value_class(&mut files, &module, function, search);
                out.entry(name).or_default().push(InstalledProvider { symbol, value });
            }
        }
    }
    for list in out.values_mut() {
        list.sort();
        list.dedup();
    }
    out
}

/// Decorator head without `@` and arguments (`@pkg.provider(scope="x")` -> `pkg.provider`).
fn head(text: &str) -> &str {
    text.trim_start_matches('@')
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
}

/// Whether a decorator head names `decorator`: as written or by its last dotted segment.
pub fn names_decorator(decorator: &str, head: &str) -> bool {
    let last = decorator.rsplit('.').next().unwrap_or(decorator);
    head == decorator || head == last || head.ends_with(&format!(".{last}"))
}

/// Dotted text of a name / attribute chain.
fn dotted(e: &Expr) -> Option<String> {
    match e {
        Expr::Name { name, .. } if !name.starts_with(LITERAL_PREFIX) => Some(name.clone()),
        Expr::Attr { object, attr, .. } => Some(format!("{}.{attr}", dotted(object)?)),
        _ => None,
    }
}

/// The literal the provider decorator call of declaration `function` passes under
/// `keyword` (`@provider(name="client")`).
fn renamed(facts: &FileFacts, function: usize, decorator: &str, keyword: &str) -> Option<String> {
    facts.flow.iter().find_map(|f| match f {
        FlowFact::Decorated {
            function: d,
            decorators,
            ..
        } if *d as usize == function => decorators.iter().find_map(|e| {
            let Expr::Call { func, kwargs, .. } = e else { return None };
            if !names_decorator(decorator, &dotted(func)?) {
                return None;
            }
            kwargs.iter().find_map(|(k, v)| match v {
                Expr::Name { name, .. } if k == keyword => name
                    .strip_prefix(LITERAL_PREFIX)
                    .filter(|n| !n.is_empty())
                    .map(str::to_string),
                _ => None,
            })
        }),
        _ => None,
    })
}

/// The library class every value of provider `function` constructs (module doc).
fn value_class(files: &mut Files<'_>, module: &Path, function: usize, search: &[PathBuf]) -> Option<String> {
    let spellings = constructed(&files.get(module)?.facts, function)?;
    let mut classes: BTreeSet<String> = BTreeSet::new();
    for spelling in spellings {
        let (file, class) = resolve_base(files, module, &spelling, search)?;
        classes.insert(qualified(Language::Python, &file, &files.get(&file)?.facts, class));
    }
    let mut classes = classes.into_iter();
    match (classes.next(), classes.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Callee spellings of the constructions every value of `function` is (returned values, for
/// a generator yielded values; a local variable stands for all of its bindings); `None` when
/// a value is anything else.
fn constructed(facts: &FileFacts, function: usize) -> Option<BTreeSet<String>> {
    let decl = facts.declarations.get(function)?;
    let generator = matches!(decl.execution, ExecutionModel::Generator | ExecutionModel::AsyncGenerator);
    let local_binds = |name: &str| -> Vec<&Expr> {
        facts
            .flow
            .iter()
            .filter_map(|f| match f {
                FlowFact::Bind {
                    target: BindTarget::Var { scope, name: n },
                    value,
                    ..
                } if *scope == Scope::Decl(function as u32) && n == name => Some(value),
                _ => None,
            })
            .collect()
    };
    let values: Vec<&Expr> = if generator {
        local_binds(YIELDED)
    } else {
        facts
            .flow
            .iter()
            .filter_map(|f| match f {
                FlowFact::Return { function: d, value } if *d as usize == function => Some(value),
                _ => None,
            })
            .collect()
    };
    if values.is_empty() {
        return None;
    }
    let mut spellings: BTreeSet<String> = BTreeSet::new();
    for value in values {
        let constructions = match value {
            Expr::Call { .. } => vec![value],
            Expr::Name { name, .. } if !name.starts_with(LITERAL_PREFIX) => {
                // A parameter is bound by the caller: not a construction the source shows.
                if decl.parameters.iter().any(|p| &p.name == name) {
                    return None;
                }
                let binds = local_binds(name);
                if binds.is_empty() {
                    return None;
                }
                binds
            }
            _ => return None,
        };
        for c in constructions {
            let Expr::Call { func, .. } = c else { return None };
            spellings.insert(dotted(func)?);
        }
    }
    Some(spellings)
}

/// Python modules (`RECORD` entries inside `root`) of the distribution named `package` and of
/// the distributions requiring it unconditionally.
fn distribution_modules(root: &Path, package: &str) -> Vec<PathBuf> {
    let wanted = crate::installed::normalize(EcosystemId::Python, package);
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dists: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().is_some_and(|x| x == "dist-info"))
        .collect();
    dists.sort();
    let mut out = Vec::new();
    for dist in dists {
        let Ok(metadata) = std::fs::read_to_string(dist.join("METADATA")) else {
            continue;
        };
        let (name, requires) = parse_metadata(&metadata);
        let name = crate::installed::normalize(EcosystemId::Python, &name);
        if name != wanted
            && !requires
                .iter()
                .any(|r| crate::installed::normalize(EcosystemId::Python, r) == wanted)
        {
            continue;
        }
        let Ok(record) = std::fs::read_to_string(dist.join("RECORD")) else {
            continue;
        };
        out.extend(record_modules(root, &record).into_iter().take(MAX_MODULES));
    }
    out
}

/// `Name` and the unconditional `Requires-Dist` names of a `METADATA` header (core
/// metadata: RFC 822 fields up to the first empty line).
fn parse_metadata(text: &str) -> (String, Vec<String>) {
    let mut name = String::new();
    let mut requires = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Name:") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Requires-Dist:") {
            let (spec, marker) = v.split_once(';').unwrap_or((v, ""));
            if marker.contains("extra") {
                continue;
            }
            let spec = spec.trim();
            let end = spec
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
                .unwrap_or(spec.len());
            if end > 0 {
                requires.push(spec[..end].to_string());
            }
        }
    }
    (name, requires)
}

/// `.py` files a `RECORD` lists inside `root` (first CSV field; entries outside the root,
/// such as scripts, are skipped).
fn record_modules(root: &Path, record: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = record
        .lines()
        .filter_map(|l| l.split(',').next())
        .map(str::trim)
        .filter(|p| p.ends_with(".py") && !p.starts_with("..") && !Path::new(p).is_absolute())
        .map(|p| root.join(p))
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
#[path = "../tests/unit/injected.rs"]
mod tests;
