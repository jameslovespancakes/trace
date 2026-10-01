//! Per-language library rules (PLAN section 4): one file per language holding its
//! [`LibrarySpec`] - the embedded table (`assets/library/<language>.json`) and, for languages
//! whose installed library source is read, the [`AdapterSpec`] telling the generic derivation
//! rules how to read that language's IR (self slots, container store / read methods,
//! invocation forms, iteration forms, constructors, module names and import resolution).
//! Everything here is a language rule (core syntax, core container protocols, module
//! systems); no library is named. Engines ask the spec ([`spec_for`], [`adapter`]), never
//! `match` a language.

use std::path::{Path, PathBuf};

use trace_core::facts::ImportKind;
use trace_core::Language;

use crate::archive;

mod bash;
mod c;
mod cpp;
mod csharp;
mod go;
mod haskell;
mod java;
pub mod javascript;
pub mod objects;
mod php;
mod python;
mod r;
mod rust;
mod scala;

/// The library rules of one language (or of languages sharing one module namespace and one
/// table: JavaScript / TypeScript / TSX).
#[derive(Debug)]
pub struct LibrarySpec {
    /// The languages served; the first names the table file and the gate entry.
    pub languages: &'static [Language],
    /// The embedded table (`assets/library/<first language>.json`).
    pub table: &'static str,
    /// Derivation from installed library source; `None`: the language's library code is
    /// not read (declared function types and the table only).
    pub adapter: Option<AdapterSpec>,
}

/// [`AdapterSpec::resolve_import`]: (importing file, target, import kind, library roots) ->
/// (library file, imported member).
pub type ResolveImport = fn(&Path, &str, ImportKind, &[PathBuf]) -> Option<(PathBuf, Option<String>)>;

/// Language data the generic derivation rules need.
#[derive(Clone, Debug)]
pub struct AdapterSpec {
    /// Source file extensions of library code (without the dot).
    pub extensions: &'static [&'static str],
    /// Positional parameters may also be passed by name (`f(x=1)`): selectors are
    /// `PosOrKw`, else `Pos`.
    pub keyword_args: bool,
    /// Methods that store their arguments into the receiver container (`append`, `push`,
    /// `<<`); with two or more arguments the first is the key (`set(k, v)`).
    pub store_methods: &'static [&'static str],
    /// Methods whose value is an element of the receiver container (`pop`, `get`, `[]`).
    pub read_methods: &'static [&'static str],
    /// Methods that call their receiver (`f.call(..)`, `f.apply(..)`, `blk.call`).
    pub invoke_methods: &'static [&'static str],
    /// Free functions that call one argument (`do.call(f, args)`, `call_user_func(f)`):
    /// (callee spelling, argument index).
    pub invoke_functions: &'static [(&'static str, u32)],
    /// Free functions whose value is (the elements of) one argument (`ipairs(t)`,
    /// `match.fun(f)`, `enumerate(xs)`): (callee spelling, argument index).
    pub identity_functions: &'static [(&'static str, u32)],
    /// Free functions whose value holds all arguments (`append(s, x)`, `c(a, b)`).
    pub container_functions: &'static [&'static str],
    /// Free functions that store their other arguments into one argument container
    /// (`table.insert(t, v)`, `array_push($a, $v)`): (callee spelling, container index).
    pub store_functions: &'static [(&'static str, u32)],
    /// Core collection methods that call a function argument / block with every element of
    /// the receiver (`each`, `forEach`, `for_each`).
    pub element_methods: &'static [&'static str],
    /// Constructor method names (besides `SymbolKind::Constructor` declarations).
    pub constructors: &'static [&'static str],
    /// Method run when an instance is called (`__call__`, `__invoke`, `call`).
    pub call_method: Option<&'static str>,
    /// Descriptor getter (`__get__`): a slot it calls makes the stored value a property.
    pub get_method: Option<&'static str>,
    /// Member-lookup hook the runtime calls with the member name when an attribute is
    /// missing (`__getattr__(self, name)`, language specification).
    pub member_hook: Option<&'static str>,
    /// `super()` call name (Python) whose value is the instance seen as its first base.
    pub super_call: Option<&'static str>,
    /// Separator between the module prefix and the qualified name in library symbols.
    pub symbol_separator: &'static str,
    /// All files of one directory share one namespace (Go package, R package `R/`).
    pub namespace_group: bool,
    /// The language's server may answer a standard-library call without a location
    /// (Python): such a call is located through the stdlib module index. Off for languages
    /// whose server always locates standard-library declarations (Go, Rust, Java,
    /// JavaScript / TypeScript, PHP, R): their index is never built.
    pub stdlib_index: bool,
    /// One function name is several overloads selected by arity (Java overloads): a call
    /// reaches every same-named function of the scope whose arity accepts it, and the
    /// summaries of one symbol are merged.
    pub clauses: bool,
    /// Free functions whose value is the member names of one argument (`Object.keys(o)`,
    /// `Object.getOwnPropertyNames(o)`): (callee spelling, argument index).
    pub key_functions: &'static [(&'static str, u32)],
    /// Free functions that define a member on one argument under the name held by another
    /// argument (`Object.defineProperty(o, k, d)`): (callee spelling, object index, key index).
    pub define_functions: &'static [(&'static str, u32, u32)],
    /// Declared types whose values are functions, with the method that calls them (the
    /// language's core functional interfaces: `Runnable.run`, `Function.apply`), matched by
    /// the type's last segment.
    pub functional_types: &'static [(&'static str, &'static str)],
    /// A library interface with exactly one abstract method is a function type (Java: a
    /// lambda can be passed wherever such an interface is expected).
    pub single_method_interfaces_are_functions: bool,
    /// Inside a method, a bare name that is no parameter but a declared field of the class
    /// is that field of `this` (Java `handler = h;`, `handler.run()`).
    pub implicit_fields: bool,
    /// Prototype object model (JavaScript): member functions of objects form classes,
    /// `this`, `arguments`, receiver-binding invocations, prototype links, module values.
    pub objects: Option<objects::ObjectModel>,
    /// Type declaration syntax of typed dispatch (`derive::typed`): Go.
    pub(crate) typed: Option<&'static crate::derive::typed::TypeSyntax>,
    /// Module prefix of a library file for its library-qualified symbols (`asyncio.events`,
    /// `express/lib/router/index`, `net/http`, `std::thread`), or `None` when the language's
    /// qualified names are already global (PHP). Arguments: the file, the library roots.
    pub module_name: fn(&Path, &[PathBuf]) -> Option<String>,
    /// The library file an import of `from` names, and the imported member (if the import
    /// binds a member rather than a module). Arguments: importing file, target, import kind,
    /// library roots.
    pub resolve_import: ResolveImport,
    /// The readable source form of a library location the server reported without one: a
    /// declaration file's implementation (`.d.ts` -> `.js`), a class file's source in a
    /// `-sources.jar` / the JDK's `src.zip` (`jdt://`, `jar:`, `jrt:` locations). `None` when
    /// no source is installed. Arguments: the location, the library roots.
    pub source_of_location: fn(&str, &[PathBuf]) -> Option<PathBuf>,
    /// The entry module of the installed package holding a file (languages whose packages
    /// have one: the JavaScript manifest `main`); loaded with the file derived, as the
    /// repository's import of the package runs it.
    pub package_entry: fn(&Path) -> Option<PathBuf>,
    /// Whether a sibling file (by name) belongs to the namespace group (Go: no test files, no
    /// files built only for another platform).
    pub namespace_member: fn(&str) -> bool,
}

pub(crate) const BASE: AdapterSpec = AdapterSpec {
    extensions: &[],
    keyword_args: false,
    store_methods: &[],
    read_methods: &[],
    invoke_methods: &[],
    invoke_functions: &[],
    identity_functions: &[],
    container_functions: &[],
    store_functions: &[],
    element_methods: &[],
    constructors: &[],
    call_method: None,
    get_method: None,
    member_hook: None,
    super_call: None,
    symbol_separator: ".",
    namespace_group: false,
    stdlib_index: false,
    clauses: false,
    key_functions: &[],
    define_functions: &[],
    functional_types: &[],
    single_method_interfaces_are_functions: false,
    implicit_fields: false,
    objects: None,
    typed: None,
    module_name: |_, _| None,
    resolve_import: |_, _, _, _| None,
    source_of_location: |_, _| None,
    package_entry: |_| None,
    namespace_member: |_| true,
};

/// Every language with library rules, in table order (the order is part of the library
/// summary cache key: `Library::context` hashes the tables in this order).
pub static ALL: [&LibrarySpec; 13] = [
    &python::SPEC,
    &javascript::SPEC,
    &rust::SPEC,
    &go::SPEC,
    &java::SPEC,
    &c::SPEC,
    &cpp::SPEC,
    &csharp::SPEC,
    &php::SPEC,
    &bash::SPEC,
    &scala::SPEC,
    &r::SPEC,
    &haskell::SPEC,
];

/// The library rules serving `language`.
pub fn spec_for(language: Language) -> Option<&'static LibrarySpec> {
    ALL.iter().copied().find(|s| s.languages.contains(&language))
}

/// The derivation adapter of `language` (None for languages without readable library source).
pub fn adapter(language: Language) -> Option<&'static AdapterSpec> {
    spec_for(language)?.adapter.as_ref()
}

/// The language whose table and gate entries serve `language` (itself unless it shares
/// another language's: TypeScript / TSX -> JavaScript).
pub fn table_language(language: Language) -> Language {
    spec_for(language).map_or(language, |s| s.languages[0])
}

/// Files sharing one namespace with `path` (the adapter's `namespace_group`), `path` first,
/// at most `limit`.
pub fn namespace_files(spec: &AdapterSpec, path: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    if !spec.namespace_group {
        return out;
    }
    if archive::split(path).is_some() {
        out.extend(
            archive::siblings(path, spec.extensions)
                .into_iter()
                .take(limit.saturating_sub(1)),
        );
        return out;
    }
    let Some(dir) = path.parent() else {
        return out;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut siblings: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p != path && p.is_file() && has_extension(p, spec.extensions))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_none_or(|n| (spec.namespace_member)(n))
        })
        .collect();
    siblings.sort();
    siblings.truncate(limit.saturating_sub(1));
    out.extend(siblings);
    out
}

/// Whether `path` has one of `extensions` (case-sensitive, R accepts both cases).
pub fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.contains(&e))
}

/// First existing file among `candidates`.
pub(crate) fn first_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_file())
}

/// Path components of `path` as strings (lossy; for layout checks only).
pub(crate) fn components(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// `path` relative to the first of `roots` that contains it.
pub(crate) fn relative_to_roots<'a>(path: &'a Path, roots: &[PathBuf]) -> Option<(&'a Path, PathBuf)> {
    roots
        .iter()
        .filter_map(|r| path.strip_prefix(r).ok().map(|rel| (rel, r.clone())))
        .max_by_key(|(_, r)| r.components().count())
}

#[cfg(test)]
#[path = "../../tests/unit/languages/mod.rs"]
mod tests;
