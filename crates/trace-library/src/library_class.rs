//! Members of a library class (rule "member of a library base").
//!
//! A repository type whose base the server located outside the index
//! (`FileSemantics::library_bases`) inherits the members of that library class. This module
//! reads the class from the syntax tree of the file the server named (a source file or a
//! stub: both declare the class's members) at the declaration position the server gave, and
//! collects the callable members it declares, then those of its bases found from its source:
//! a class of the same file, or a class an import of that file binds (followed through
//! re-exports), breadth-first, first declaration of a name wins, at most [`MAX_CLASSES`]
//! classes. Bases that cannot be found add nothing (their members are simply not known).
//!
//! Python data model: a class declaring `__getattribute__` intercepts every member access,
//! so no member of it (or of its subclasses) is known ([`LibraryClass`] is `None`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use trace_core::facts::{FileFacts, ImportKind, Scope};
use trace_core::semantics::LibraryFile;
use trace_core::Language;
use trace_syntax::language_rules::{rules, ModulePaths};

use crate::derive::{FsLoader, SourceLoader};
use crate::{languages, symbol, Library};

/// Classes read per library class at most (the class and its ancestors).
const MAX_CLASSES: usize = 16;
/// Re-export steps followed to find an imported class.
const MAX_REEXPORTS: usize = 4;

/// A library class and the callable members it has.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryClass {
    /// Library-qualified class (`pkg.mod.Client`).
    pub symbol: String,
    /// Member name -> library-qualified member that lookup finds first (`pkg.mod.Base.get`).
    pub members: BTreeMap<String, String>,
}

impl Library {
    /// The library class declared at (`line`, `column`, 0-based) of `file` (a base the
    /// server located), with its members; `None` when no type is declared there or its
    /// members cannot be known.
    pub fn library_class(&self, file: &LibraryFile, line: u32, column: u32) -> Option<LibraryClass> {
        let path = PathBuf::from(&file.path);
        let language = trace_core::languages::from_path(&path).unwrap_or(file.language);
        let roots = self.root_paths(language);
        class_at(&FsLoader, language, &path, line, column, &roots)
    }
}

/// A parsed library file.
pub(crate) struct Parsed {
    pub(crate) facts: FileFacts,
    pub(crate) source: Vec<u8>,
}

/// Parsed files of one lookup (`None`: unreadable / unparsable).
pub(crate) struct Files<'l> {
    loader: &'l dyn SourceLoader,
    language: Language,
    parsed: HashMap<PathBuf, Option<Parsed>>,
}

impl<'l> Files<'l> {
    pub(crate) fn new(loader: &'l dyn SourceLoader, language: Language) -> Files<'l> {
        Files {
            loader,
            language,
            parsed: HashMap::new(),
        }
    }

    pub(crate) fn get(&mut self, path: &Path) -> Option<&Parsed> {
        if !self.parsed.contains_key(path) {
            let parsed = self.parse(path);
            self.parsed.insert(path.to_path_buf(), parsed);
        }
        self.parsed.get(path).and_then(Option::as_ref)
    }

    /// Parse `paths` not parsed yet, in parallel (what [`Files::get`] would parse one by one).
    pub(crate) fn preload(&mut self, paths: &[PathBuf]) {
        let todo: BTreeSet<&PathBuf> = paths.iter().filter(|p| !self.parsed.contains_key(*p)).collect();
        let this = &*self;
        let parsed: Vec<(PathBuf, Option<Parsed>)> =
            todo.into_par_iter().map(|p| (p.clone(), this.parse(p))).collect();
        self.parsed.extend(parsed);
    }

    fn parse(&self, path: &Path) -> Option<Parsed> {
        self.loader.read(path).and_then(|source| {
            let path_text = path.to_string_lossy();
            trace_syntax::extract(trace_syntax::SourceInput {
                path: &path_text,
                language: self.language,
                source: &source,
            })
            .ok()
            .map(|facts| Parsed { facts, source })
        })
    }
}

/// [`Library::library_class`] over a source loader.
pub fn class_at(
    loader: &dyn SourceLoader,
    language: Language,
    path: &Path,
    line: u32,
    column: u32,
    roots: &[PathBuf],
) -> Option<LibraryClass> {
    let mut files = Files::new(loader, language);
    let index = {
        let parsed = files.get(path)?;
        let index = symbol::declaration_at(&parsed.facts, &parsed.source, line, column)?;
        if !parsed.facts.declarations[index].kind.is_type() {
            return None;
        }
        index
    };
    let symbol = qualified(language, path, &files.get(path)?.facts, index);
    let mut members: BTreeMap<String, String> = BTreeMap::new();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::from([(path.to_path_buf(), index)]);
    let mut seen: HashSet<(PathBuf, usize)> = HashSet::new();
    while let Some((file, class)) = queue.pop_front() {
        if !seen.insert((file.clone(), class)) {
            continue;
        }
        if seen.len() > MAX_CLASSES {
            break;
        }
        let Some(parsed) = files.get(&file) else { continue };
        let owner = qualified(language, &file, &parsed.facts, class);
        let declared: Vec<String> = parsed
            .facts
            .declarations
            .iter()
            .filter(|m| m.parent == Some(class as u32) && m.kind.is_callable() && !m.name.starts_with('<'))
            .map(|m| m.name.clone())
            .collect();
        if rules(language)
            .attribute_hook
            .is_some_and(|hook| declared.iter().any(|m| m == hook))
        {
            return None;
        }
        for name in declared {
            let qualified_member = format!("{owner}.{name}");
            members.entry(name).or_insert(qualified_member);
        }
        let bases = parsed.facts.declarations[class].bases.clone();
        for base in &bases {
            if let Some(found) = resolve_base(&mut files, &file, base, roots) {
                queue.push_back(found);
            }
        }
    }
    Some(LibraryClass { symbol, members })
}

/// `<module>.<qualified name>` of declaration `index` of a library file.
pub(crate) fn qualified(language: Language, path: &Path, facts: &FileFacts, index: usize) -> String {
    let decl = &facts.declarations[index];
    match languages::adapter(language).and_then(|a| (a.module_name)(path, &[])) {
        Some(m) => format!("{m}.{}", decl.qualified_name),
        None => decl.qualified_name.clone(),
    }
}

/// Name segments of a base spelling without generic arguments (`pkg.Base[T]` ->
/// [`pkg`, `Base`]); `None` for computed bases (`make_base()`).
fn base_path(spelling: &str) -> Option<Vec<&str>> {
    if spelling.contains('(') {
        return None;
    }
    let head = spelling.split(['[', '<']).next().unwrap_or_default().trim();
    let parts: Vec<&str> = head.split('.').map(str::trim).collect();
    (!parts.is_empty() && parts.iter().all(|p| !p.is_empty())).then_some(parts)
}

/// Module-level type declaration named `name` of a parsed file.
fn module_type(facts: &FileFacts, name: &str) -> Option<usize> {
    facts.declarations.iter().enumerate().position(|(i, d)| {
        d.kind.is_type()
            && d.name == name
            && facts.module_decl != Some(i as u32)
            && d.parent.is_none_or(|p| facts.module_decl == Some(p))
    })
}

/// The class a base spelling of a class in `file` denotes: a class of the same file, else
/// the class an import of the file binds (Python: dotted spellings through module imports).
pub(crate) fn resolve_base(
    files: &mut Files<'_>,
    file: &Path,
    spelling: &str,
    roots: &[PathBuf],
) -> Option<(PathBuf, usize)> {
    let parts = base_path(spelling)?;
    if parts.len() == 1 {
        if let Some(i) = module_type(&files.get(file)?.facts, parts[0]) {
            return Some((file.to_path_buf(), i));
        }
    } else if rules(files.language).modules != ModulePaths::DottedModules {
        return None;
    }
    imported_class(files, file, &parts, roots, MAX_REEXPORTS)
}

/// The class that `parts` (a name bound by an import of `file`, then attribute segments)
/// denotes, following re-exports at most `depth` times.
fn imported_class(
    files: &mut Files<'_>,
    file: &Path,
    parts: &[&str],
    roots: &[PathBuf],
    depth: usize,
) -> Option<(PathBuf, usize)> {
    let language = files.language;
    let import = files
        .get(file)?
        .facts
        .imports
        .iter()
        .find(|i| i.scope == Scope::Module && i.kind != ImportKind::Wildcard && i.local == parts[0])
        .cloned()?;
    let target = std::iter::once(import.target.as_str())
        .chain(parts[1..].iter().copied())
        .collect::<Vec<_>>()
        .join(".");
    let resolve = languages::adapter(language)?.resolve_import;
    let (module, member) = match resolve(file, &target, import.kind, roots) {
        Some((module, Some(member))) => (module, member),
        // `from . import Base`: a name of the package module itself.
        _ => {
            let cut = target.rfind('.')?;
            let parent = if target[..cut].chars().all(|c| c == '.') {
                &target[..=cut]
            } else {
                &target[..cut]
            };
            let (module, member) = resolve(file, parent, import.kind, roots)?;
            if member.is_some() {
                return None;
            }
            (module, target[cut + 1..].to_string())
        }
    };
    if let Some(i) = module_type(&files.get(&module)?.facts, &member) {
        return Some((module, i));
    }
    if depth == 0 || module == file {
        return None;
    }
    imported_class(files, &module, &[member.as_str()], roots, depth - 1)
}

#[cfg(test)]
#[path = "../tests/unit/library_class.rs"]
mod tests;
