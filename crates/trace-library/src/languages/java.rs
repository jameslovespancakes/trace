//! Java adapter: library source comes from the `-sources.jar` a Maven / Gradle cache keeps
//! next to a dependency jar and from the JDK's `lib/src.zip`, read as `<archive>!/<entry>`
//! paths ([`crate::archive`]). jdtls reports library declarations as class-file locations
//! (`jdt://contents/...`, `jar:file:...!/...class`, `jrt:/<module>/...class`), which
//! [`source_of_location`] maps to the source entry of the class (a nested class `Outer$Inner`
//! lives in `Outer.java`). `this` slots come from `ImplicitSelf`; the classes of one package
//! share a namespace; overloads are clauses of one name; calling the single method of a
//! parameter / field whose declared type is a functional interface calls the passed function
//! (the language's core functional interfaces below, and any library interface with exactly
//! one abstract method).

use std::path::{Path, PathBuf};

use trace_core::facts::ImportKind;
use trace_core::Language;

use super::{AdapterSpec, LibrarySpec, BASE};
use crate::archive;

/// Core functional interfaces of the Java platform (`java.lang.Runnable`,
/// `java.util.concurrent.Callable`, `java.util.Comparator`, `java.util.function.*`) and the
/// method that runs them. They are the language's function types: a lambda or method
/// reference passed where one is expected is run by that method.
const FUNCTIONAL_TYPES: &[(&str, &str)] = &[
    ("Runnable", "run"),
    ("Callable", "call"),
    ("Comparator", "compare"),
    ("Supplier", "get"),
    ("Consumer", "accept"),
    ("BiConsumer", "accept"),
    ("Function", "apply"),
    ("BiFunction", "apply"),
    ("UnaryOperator", "apply"),
    ("BinaryOperator", "apply"),
    ("Predicate", "test"),
    ("BiPredicate", "test"),
    ("BooleanSupplier", "getAsBoolean"),
    ("IntFunction", "apply"),
    ("IntPredicate", "test"),
    ("IntConsumer", "accept"),
    ("IntSupplier", "getAsInt"),
    ("IntUnaryOperator", "applyAsInt"),
    ("IntBinaryOperator", "applyAsInt"),
    ("IntToLongFunction", "applyAsLong"),
    ("IntToDoubleFunction", "applyAsDouble"),
    ("ToIntFunction", "applyAsInt"),
    ("ToIntBiFunction", "applyAsInt"),
    ("ToLongFunction", "applyAsLong"),
    ("ToLongBiFunction", "applyAsLong"),
    ("ToDoubleFunction", "applyAsDouble"),
    ("ToDoubleBiFunction", "applyAsDouble"),
    ("LongFunction", "apply"),
    ("LongPredicate", "test"),
    ("LongConsumer", "accept"),
    ("LongSupplier", "getAsLong"),
    ("LongUnaryOperator", "applyAsLong"),
    ("LongBinaryOperator", "applyAsLong"),
    ("DoubleFunction", "apply"),
    ("DoublePredicate", "test"),
    ("DoubleConsumer", "accept"),
    ("DoubleSupplier", "getAsDouble"),
    ("DoubleUnaryOperator", "applyAsDouble"),
    ("DoubleBinaryOperator", "applyAsDouble"),
    ("ObjIntConsumer", "accept"),
    ("ObjLongConsumer", "accept"),
    ("ObjDoubleConsumer", "accept"),
];

pub(crate) static SPEC: LibrarySpec = LibrarySpec {
    languages: &[Language::Java],
    table: include_str!("../../../../assets/library/java.json"),
    adapter: Some(AdapterSpec {
        extensions: &["java"],
        store_methods: &["add", "addFirst", "addLast", "offer", "offerFirst", "offerLast", "push", "put"],
        read_methods: &[
            "get",
            "poll",
            "pollFirst",
            "pollLast",
            "pop",
            "peek",
            "take",
            "remove",
            "values",
        ],
        element_methods: &["forEach"],
        namespace_group: true,
        clauses: true,
        functional_types: FUNCTIONAL_TYPES,
        single_method_interfaces_are_functions: true,
        implicit_fields: true,
        module_name,
        resolve_import,
        source_of_location,
        ..BASE
    }),
};

/// Package of a Java source entry (`com/acme/Pool.java` -> `com.acme`; the JDK's module
/// directory `java.base/java/util/List.java` is not part of the package). Plain files have no
/// module prefix.
fn module_name(path: &Path, _roots: &[PathBuf]) -> Option<String> {
    let (_, entry) = archive::split(path)?;
    let mut parts: Vec<&str> = entry.split('/').filter(|p| !p.is_empty()).collect();
    parts.pop()?;
    if parts.len() > 1 && parts[0].contains('.') {
        parts.remove(0);
    }
    (!parts.is_empty()).then(|| parts.join("."))
}

/// Source archives of the JDK among the library roots (`<jdk>/lib/src.zip`, `<jdk>/src.zip`).
fn jdk_sources(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .flat_map(|r| [r.join("lib").join("src.zip"), r.join("src.zip")])
        .filter(|p| p.is_file())
        .collect()
}

/// `import a.b.C;`: `a/b/C.java` in the importing archive, else in the JDK sources; the
/// member is the class. Wildcard imports and nested-class imports are not followed.
fn resolve_import(
    from: &Path,
    target: &str,
    kind: ImportKind,
    roots: &[PathBuf],
) -> Option<(PathBuf, Option<String>)> {
    if kind == ImportKind::Wildcard || target.is_empty() {
        return None;
    }
    let class = target.rsplit('.').next()?.to_string();
    let entry = format!("{}.java", target.replace('.', "/"));
    if let Some((jar, _)) = archive::split(from) {
        if archive::has_entry(&jar, &entry) {
            return Some((archive::entry_path(&jar, &entry), Some(class)));
        }
    }
    for zip in jdk_sources(roots) {
        if let Some(found) = archive::find_suffix(&zip, &entry) {
            return Some((archive::entry_path(&zip, &found), Some(class)));
        }
    }
    None
}

/// `%XX` escapes decoded (invalid escapes kept as written).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A class-file location: the jar (None for JDK modules), the JDK module (if named) and the
/// class entry path without extension (`com/acme/Pool$Task`).
#[derive(Debug, PartialEq, Eq)]
struct ClassLocation {
    jar: Option<PathBuf>,
    module: Option<String>,
    class: String,
}

/// `jdt://contents/<jar or module>/<package>/<Class>.class?=<project>/<escaped jar path><...`
fn parse_jdt(uri: &str) -> Option<ClassLocation> {
    let rest = uri.strip_prefix("jdt://contents/")?;
    let (path_part, query) = rest.split_once("?=").unwrap_or((rest, ""));
    let path_part = percent_decode(path_part);
    let segs: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();
    let container = *segs.first()?;
    let class_file = *segs.last()?;
    let package = if segs.len() >= 3 { segs[1] } else { "" };
    let class = class_file.strip_suffix(".class").unwrap_or(class_file);
    if class.is_empty() || segs.len() < 2 {
        return None;
    }
    let class = if package.is_empty() {
        class.to_string()
    } else {
        format!("{}/{class}", package.replace('.', "/"))
    };
    if !container.to_ascii_lowercase().ends_with(".jar") {
        return Some(ClassLocation {
            jar: None,
            module: Some(container.to_string()),
            class,
        });
    }
    let decoded = percent_decode(query).replace("\\/", "/").replace('\\', "/");
    let after_project = decoded.split_once('/').map(|(_, p)| p).unwrap_or(&decoded);
    let jar = after_project.split('<').next().unwrap_or("").to_string();
    Some(ClassLocation {
        jar: (!jar.is_empty()).then(|| PathBuf::from(jar)),
        module: None,
        class,
    })
}

/// `jar:file:///C:/x/lib.jar!/pkg/Cls.class` and `jrt:/java.base/java/util/List.class`.
fn parse_jar_uri(uri: &str) -> Option<ClassLocation> {
    if let Some(rest) = uri.strip_prefix("jrt:") {
        let decoded = percent_decode(rest);
        let (module, entry) = decoded.trim_start_matches('/').split_once('/')?;
        let class = entry.strip_suffix(".class").unwrap_or(entry).to_string();
        return Some(ClassLocation {
            jar: None,
            module: Some(module.to_string()),
            class,
        });
    }
    let rest = uri.strip_prefix("jar:")?;
    let rest = rest.strip_prefix("file:").unwrap_or(rest);
    let decoded = percent_decode(rest);
    let (jar, entry) = decoded.split_once("!/")?;
    // `file:///C:/x` keeps a leading slash before the drive letter.
    let jar = jar.trim_start_matches("//");
    let jar = if jar.len() > 2 && jar.starts_with('/') && jar.as_bytes().get(2) == Some(&b':') {
        &jar[1..]
    } else {
        jar
    };
    Some(ClassLocation {
        jar: Some(PathBuf::from(jar)),
        module: None,
        class: entry.strip_suffix(".class").unwrap_or(entry).to_string(),
    })
}

/// The sources jar of a jar: `<stem>-sources.jar` next to it (Maven), or in a sibling hash
/// directory of the same version directory (Gradle `files-2.1/<g>/<a>/<v>/<hash>/`).
fn sources_jar(jar: &Path) -> Option<PathBuf> {
    let stem = jar.file_stem()?.to_string_lossy().into_owned();
    let name = format!("{stem}-sources.jar");
    let dir = jar.parent()?;
    let direct = dir.join(&name);
    if direct.is_file() {
        return Some(direct);
    }
    let version_dir = dir.parent()?;
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(version_dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join(&name))
        .filter(|p| p.is_file())
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

/// The source entry (`<archive>!/<pkg>/<Outer>.java`) of a class-file location reported by
/// the server, or a plain `.java` path as it is; `None` when no source is installed.
fn source_of_location(location: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let parsed = parse_jdt(location).or_else(|| parse_jar_uri(location));
    let Some(loc) = parsed else {
        let path = Path::new(location);
        return (path.extension().is_some_and(|e| e == "java") && path.is_file()).then(|| path.to_path_buf());
    };
    // A nested class lives in the file of its outermost class.
    let (dir, file) = match loc.class.rsplit_once('/') {
        Some((d, f)) => (format!("{d}/"), f),
        None => (String::new(), loc.class.as_str()),
    };
    let outer = file.split('$').next().unwrap_or(file);
    if outer.is_empty() {
        return None;
    }
    let entry = format!("{dir}{outer}.java");
    if let Some(jar) = &loc.jar {
        let sources = sources_jar(jar)?;
        return archive::has_entry(&sources, &entry).then(|| archive::entry_path(&sources, &entry));
    }
    for zip in jdk_sources(roots) {
        if let Some(module) = &loc.module {
            let in_module = format!("{module}/{entry}");
            if archive::has_entry(&zip, &in_module) {
                return Some(archive::entry_path(&zip, &in_module));
            }
        }
        if archive::has_entry(&zip, &entry) {
            return Some(archive::entry_path(&zip, &entry));
        }
    }
    None
}

#[cfg(test)]
#[path = "../../tests/unit/languages/java.rs"]
mod tests;
