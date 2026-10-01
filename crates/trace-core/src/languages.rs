//! The only list of languages: identity (stable name, human name, file extensions, shebang
//! interpreters, call-namespace family) and support levels.
//!
//! Every other crate asks this table ([`info`], [`from_path`], [`from_extension`],
//! [`from_shebang`], [`same_family`]) instead of keeping its own extension, shebang or family
//! list. Adding a language = one [`Language`] variant + one row of the table (then the
//! per-language files of trace-syntax, trace-semantic, trace-env and trace-library).

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Every language `trace` inventories. Whether a grammar is compiled in is reported by
/// `trace_syntax::grammar(lang).is_some()`; semantic support by `trace-semantic` probes.
/// The variant order is the order of [`Language::ALL`] and of the identity table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Python,
    /// JavaScript including JSX (`.js .mjs .cjs .jsx`).
    JavaScript,
    TypeScript,
    Tsx,
    Rust,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Php,
    Bash,
    Scala,
    R,
    Haskell,
    Julia,
    OCaml,
    // Inventoried only: no grammar crate compatible with the shared tree-sitter version.
    VisualBasic,
    Erlang,
    FSharp,
    Clojure,
    PowerShell,
    Sql,
    Fortran,
    Vue,
    Svelte,
    // Contract files (inventoried; read by `trace-bridge` through `SourceStore`, never
    // executed): `.proto`, `.graphql`/`.gql`, OpenAPI/Swagger documents.
    Proto,
    GraphQl,
    /// JSON/YAML files named `openapi.*`, `swagger.*`, `*.openapi.*` or `*.swagger.*`.
    OpenApi,
}

/// Languages sharing one call namespace: a name declared in one member language is callable
/// by name from the others (JS and TS files import each other; C++ calls C; JVM and CLR
/// languages share their runtime's symbol namespace).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    /// JavaScript, TypeScript, Tsx, Vue, Svelte.
    JavaScript,
    /// C, C++.
    C,
    /// Java, Scala, Clojure.
    Jvm,
    /// C#, F#, Visual Basic, PowerShell.
    Clr,
}

/// How the argument count of a call is checked against a declaration's positional parameter
/// list `required..=max` ([`crate::facts::arity_accepts`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arity {
    /// Not decided from syntax (any count accepted at run time, defaults / named / variadic /
    /// curried parameters the lists do not mark).
    Undecided,
    /// `required <= args <= max`; the receiver is never in the list.
    Exact,
    /// `required <= args <= max`. Through a receiver, a first parameter named `bound` is bound
    /// by it; any other first parameter may or may not be (instance vs. class / static call),
    /// so the call is accepted when either reading accepts it.
    ReceiverMayBindFirst { bound: &'static str },
    /// `required <= args <= max`. Method-call syntax binds a first parameter named `name`;
    /// method-call syntax on a declaration without it is undecided.
    ReceiverBinds { name: &'static str },
    /// `args >= required` (extra arguments are accepted).
    AtLeastRequired,
}

/// Identity of one language.
#[derive(Debug)]
pub struct LanguageInfo {
    pub language: Language,
    /// Stable lowercase name (also the serde name).
    pub name: &'static str,
    /// Human name used in messages ("C++", "C#", "TypeScript" also for Tsx).
    pub display: &'static str,
    /// File extensions without dot, lowercase (matched case-insensitively by [`from_extension`]).
    pub extensions: &'static [&'static str],
    /// Interpreter names of `#!` lines of extension-less scripts ([`from_shebang`]).
    pub shebangs: &'static [&'static str],
    /// Shared call namespace, if any.
    pub family: Option<Family>,
    /// One of the 15 code languages trace analyses with a language server (the 14 languages
    /// plus `Tsx`, served with TypeScript).
    pub code: bool,
    /// servers + server runtimes installed automatically on first need.
    pub default: bool,
    /// Argument-count rule of calls.
    pub arity: Arity,
    /// One type declares several callables of one name that are one API: overloads and
    /// accessor pairs (property getter / setter, `get` / `set`). Not where a type's same-named
    /// methods come from different traits or are distinct (Rust, Go).
    pub overload_groups: bool,
    /// LSP `languageId` of its documents (`textDocument/didOpen`).
    pub lsp_id: &'static str,
}

const fn row(
    language: Language,
    name: &'static str,
    display: &'static str,
    extensions: &'static [&'static str],
    family: Option<Family>,
) -> LanguageInfo {
    LanguageInfo {
        language,
        name,
        display,
        extensions,
        shebangs: &[],
        family,
        code: false,
        default: false,
        arity: Arity::Undecided,
        overload_groups: false,
        lsp_id: name,
    }
}

const fn code(mut info: LanguageInfo, default: bool) -> LanguageInfo {
    info.code = true;
    info.default = default;
    info
}

const fn shebangs(mut info: LanguageInfo, interpreters: &'static [&'static str]) -> LanguageInfo {
    info.shebangs = interpreters;
    info
}

const fn arity(mut info: LanguageInfo, rule: Arity) -> LanguageInfo {
    info.arity = rule;
    info
}

const fn overloads(mut info: LanguageInfo) -> LanguageInfo {
    info.overload_groups = true;
    info
}

const fn lsp_id(mut info: LanguageInfo, id: &'static str) -> LanguageInfo {
    info.lsp_id = id;
    info
}

use Family as F;
use Language as L;

/// The table, in [`Language`] variant order (checked at compile time).
static INFO: [LanguageInfo; 29] = [
    code(
        overloads(arity(
            shebangs(row(L::Python, "python", "Python", &["py", "pyi"], None), &["python", "pypy"]),
            Arity::ReceiverMayBindFirst { bound: "cls" },
        )),
        true,
    ),
    code(
        overloads(shebangs(
            row(L::JavaScript, "javascript", "JavaScript", &["js", "mjs", "cjs", "jsx"], Some(F::JavaScript)),
            &["node", "nodejs"],
        )),
        true,
    ),
    code(
        overloads(row(L::TypeScript, "typescript", "TypeScript", &["ts", "mts", "cts"], Some(F::JavaScript))),
        true,
    ),
    code(
        overloads(lsp_id(row(L::Tsx, "tsx", "TypeScript", &["tsx"], Some(F::JavaScript)), "typescriptreact")),
        true,
    ),
    code(arity(row(L::Rust, "rust", "Rust", &["rs"], None), Arity::ReceiverBinds { name: "self" }), true),
    code(row(L::Go, "go", "Go", &["go"], None), true),
    code(overloads(arity(row(L::Java, "java", "Java", &["java"], Some(F::Jvm)), Arity::Exact)), true),
    code(row(L::C, "c", "C", &["c", "h"], Some(F::C)), true),
    code(overloads(row(L::Cpp, "cpp", "C++", &["cpp", "cc", "cxx", "hpp", "hh", "hxx"], Some(F::C))), true),
    code(overloads(row(L::CSharp, "csharp", "C#", &["cs"], Some(F::Clr))), true),
    code(arity(row(L::Php, "php", "PHP", &["php"], None), Arity::AtLeastRequired), false),
    code(
        shebangs(
            lsp_id(row(L::Bash, "bash", "Bash", &["sh", "bash"], None), "shellscript"),
            &["sh", "bash", "dash", "ksh", "mksh", "zsh", "ash"],
        ),
        true,
    ),
    code(overloads(row(L::Scala, "scala", "Scala", &["scala", "sc"], Some(F::Jvm))), false),
    code(row(L::R, "r", "R", &["r"], None), false),
    code(row(L::Haskell, "haskell", "Haskell", &["hs"], None), false),
    row(L::Julia, "julia", "Julia", &["jl"], None),
    row(L::OCaml, "ocaml", "Ocaml", &["ml", "mli"], None),
    lsp_id(row(L::VisualBasic, "visualbasic", "Visualbasic", &["vb"], Some(F::Clr)), "vb"),
    row(L::Erlang, "erlang", "Erlang", &["erl"], None),
    row(L::FSharp, "fsharp", "Fsharp", &["fs", "fsx"], Some(F::Clr)),
    row(L::Clojure, "clojure", "Clojure", &["clj", "cljs", "cljc"], Some(F::Jvm)),
    row(L::PowerShell, "powershell", "Powershell", &["ps1"], Some(F::Clr)),
    row(L::Sql, "sql", "Sql", &["sql"], None),
    row(L::Fortran, "fortran", "Fortran", &["f90", "f95"], None),
    row(L::Vue, "vue", "Vue", &["vue"], Some(F::JavaScript)),
    row(L::Svelte, "svelte", "Svelte", &["svelte"], Some(F::JavaScript)),
    row(L::Proto, "proto", "Proto", &["proto"], None),
    row(L::GraphQl, "graphql", "Graphql", &["graphql", "gql"], None),
    // Named by file name, not extension ([`from_path`]).
    row(L::OpenApi, "openapi", "Openapi", &[], None),
];

// [`info`] indexes [`INFO`] by variant: the rows must follow the variant order.
const _: () = {
    let mut i = 0;
    while i < INFO.len() {
        assert!(INFO[i].language as usize == i && Language::ALL[i] as usize == i);
        i += 1;
    }
};

/// The identity row of `language`.
pub fn info(language: Language) -> &'static LanguageInfo {
    &INFO[language as usize]
}

/// Map a file extension (without dot, any case) to a language.
pub(crate) fn from_extension(ext: &str) -> Option<Language> {
    INFO.iter()
        .find(|i| i.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
        .map(|i| i.language)
}

/// Map a path to a language by extension; JSON/YAML files are OpenAPI contracts only when
/// their file name says so (`openapi.yaml`, `swagger.json`, `api.openapi.yml`).
pub fn from_path(path: &Path) -> Option<Language> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    if matches!(ext.to_ascii_lowercase().as_str(), "json" | "yaml" | "yml") {
        let stem = path.file_stem().and_then(|s| s.to_str())?.to_ascii_lowercase();
        let named = |n: &str| stem == n || stem.ends_with(&format!(".{n}"));
        return (named("openapi") || named("swagger")).then_some(Language::OpenApi);
    }
    from_extension(ext)
}

/// The language of the interpreter named by `first_line` (the `#!` line of an extension-less
/// script), if any.
///
/// Read structurally as a byte prefix: `#!`, optional blanks, the interpreter path, then its
/// arguments. When the interpreter is `env`, its options (`-S`, `-i`, `-u NAME`, ...) and
/// `NAME=value` assignments are skipped and the next word is the interpreter. The file name
/// of the interpreter, without `.exe` and a version suffix (`python3.12`, `ksh93`), is looked
/// up in [`LanguageInfo::shebangs`]. Anything else (or no `#!`) is not a code file for trace.
pub fn from_shebang(first_line: &[u8]) -> Option<Language> {
    let rest = first_line.strip_prefix(b"#!")?;
    let text = std::str::from_utf8(rest).ok()?;
    let text = text.trim_end_matches(['\r', '\n']);
    let mut words = text.split([' ', '\t']).filter(|w| !w.is_empty());
    let interpreter = words.next()?;
    let mut name = crate::relpath::last_component(interpreter);
    if name == "env" {
        name = env_target(&mut words)?;
    }
    let name = name.strip_suffix(".exe").unwrap_or(name);
    // Version suffixes: `python3.12`, `ksh93`; `python3-dbg` is not stripped.
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    let base = if base.is_empty() { name } else { base };
    INFO.iter().find(|i| i.shebangs.contains(&base)).map(|i| i.language)
}

/// The interpreter word after `env` and its options / assignments.
fn env_target<'a>(words: &mut impl Iterator<Item = &'a str>) -> Option<&'a str> {
    // `env` options that take a separate value.
    const WITH_VALUE: &[&str] = &["-u", "--unset", "-C", "--chdir", "-P", "--split-string-path"];
    while let Some(word) = words.next() {
        if WITH_VALUE.contains(&word) {
            words.next()?;
            continue;
        }
        if word.starts_with('-') {
            // `-S` / `-i` / `--ignore-environment` / `-S"bash -e"` spelled together: `-Sbash`.
            if let Some(joined) = word.strip_prefix("-S").filter(|s| !s.is_empty()) {
                return Some(crate::relpath::last_component(joined.trim_matches(['"', '\''])));
            }
            continue;
        }
        if word.contains('=') {
            continue;
        }
        return Some(crate::relpath::last_component(word.trim_matches(['"', '\''])));
    }
    None
}

/// Whether `a` and `b` share one call namespace: the same language or the same [`Family`].
pub fn same_family(a: Language, b: Language) -> bool {
    a == b || info(a).family.is_some_and(|f| info(b).family == Some(f))
}

/// Whether `language` belongs to `family` (JavaScript / TypeScript / TSX; C / C++; ...).
pub fn in_family(language: Language, family: Family) -> bool {
    info(language).family == Some(family)
}

/// Whether `a` and `b` share one module / type namespace: the same language, or both in the
/// [`Family::JavaScript`] family (JS and TS files import each other) or both in the
/// [`Family::C`] family (one linker namespace). JVM and CLR languages share calls
/// ([`same_family`]) but keep their own modules.
pub fn same_module_namespace(a: Language, b: Language) -> bool {
    a == b || (same_family(a, b) && matches!(info(a).family, Some(Family::JavaScript | Family::C)))
}

impl Language {
    /// All languages in display order.
    pub const ALL: [Language; 29] = [
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Tsx,
        Language::Rust,
        Language::Go,
        Language::Java,
        Language::C,
        Language::Cpp,
        Language::CSharp,
        Language::Php,
        Language::Bash,
        Language::Scala,
        Language::R,
        Language::Haskell,
        Language::Julia,
        Language::OCaml,
        Language::VisualBasic,
        Language::Erlang,
        Language::FSharp,
        Language::Clojure,
        Language::PowerShell,
        Language::Sql,
        Language::Fortran,
        Language::Vue,
        Language::Svelte,
        Language::Proto,
        Language::GraphQl,
        Language::OpenApi,
    ];

    /// Stable lowercase name (also the serde name).
    pub const fn as_str(self) -> &'static str {
        INFO[self as usize].name
    }

    /// Contract languages (bridge inputs; no symbols of their own).
    pub const fn is_contract(self) -> bool {
        matches!(self, Language::Proto | Language::GraphQl | Language::OpenApi)
    }

    /// Human name used in messages: "Python", "JavaScript", "TypeScript" (also for Tsx),
    /// "C++", "C#", "PHP", ...; other variants: their `as_str()` capitalised.
    pub const fn display_name(self) -> &'static str {
        INFO[self as usize].display
    }

    /// Id used by `trace status --install <id>`: `as_str()`, except Tsx -> "typescript".
    pub const fn install_id(self) -> &'static str {
        match self {
            Language::Tsx => "typescript",
            other => other.as_str(),
        }
    }

    /// Parse an install argument: install ids plus the aliases c# c++ cs js ts py golang
    /// sh (case-insensitive). `None` for unknown arguments and non-code languages.
    pub fn from_install_arg(arg: &str) -> Option<Language> {
        let arg = arg.trim().to_ascii_lowercase();
        let alias = match arg.as_str() {
            "c#" | "cs" => Some(Language::CSharp),
            "c++" => Some(Language::Cpp),
            "js" => Some(Language::JavaScript),
            "ts" => Some(Language::TypeScript),
            "py" => Some(Language::Python),
            "golang" => Some(Language::Go),
            "sh" => Some(Language::Bash),
            _ => None,
        };
        alias.or_else(|| {
            Language::ALL
                .iter()
                .copied()
                .filter(|l| l.is_code() && *l != Language::Tsx)
                .find(|l| l.install_id() == arg)
        })
    }

    /// True for the 15 code languages ([`LanguageInfo::code`]).
    pub const fn is_code(self) -> bool {
        INFO[self as usize].code
    }

    /// The default set ([`LanguageInfo::default`]): Python, JavaScript, TypeScript, Tsx, Java,
    /// C#, C, C++, Go, Rust, Bash. Every other code language is installed only on request.
    pub const fn is_default(self) -> bool {
        INFO[self as usize].default
    }

    /// Python type-stub files (`.pyi`) are declarations only; the sibling `.py` runs.
    pub fn is_python_stub_path(path: &str) -> bool {
        // Byte comparison: slicing the `str` could split a multi-byte character.
        let bytes = path.as_bytes();
        bytes.len() > 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".pyi")
    }
}

/// The default set in `trace status --install default` order (Tsx is served with TypeScript).
pub const DEFAULT_LANGUAGES: [Language; 10] = [
    Language::Python,
    Language::JavaScript,
    Language::TypeScript,
    Language::Java,
    Language::CSharp,
    Language::C,
    Language::Cpp,
    Language::Go,
    Language::Rust,
    Language::Bash,
];

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Language {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Language::ALL
            .iter()
            .copied()
            .find(|l| l.as_str().eq_ignore_ascii_case(s))
            .ok_or_else(|| format!("unknown language: {s}"))
    }
}

/// How well `trace` understands a language in the current environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SupportLevel {
    /// A semantic backend (compiler / language server) ran and produced proven edges.
    Semantic,
    /// Not set up yet (pending language or sub-project): analysed on first use.
    Pending,
    /// Counted and listed; no grammar compiled in, no symbols.
    Inventoried,
}

impl SupportLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            SupportLevel::Semantic => "semantic",
            SupportLevel::Pending => "pending",
            SupportLevel::Inventoried => "inventoried",
        }
    }
}

impl fmt::Display for SupportLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-language support record reported by `trace status` and stored in the index.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LanguageSupport {
    pub language: Language,
    /// Number of inventoried source files in this language.
    pub files: u32,
    /// Effective level achieved in the last index build.
    pub level: SupportLevel,
    /// Semantic backend id that served the language (`pyright`, `typescript`, `rust-analyzer`, `lsp:gopls`, ...).
    pub backend: Option<String>,
    /// Whether that backend's executable/tooling was found and trusted.
    pub backend_available: bool,
    /// Human-readable explanation, e.g. "fixtures only; set up on first use".
    pub reason: String,
}

#[cfg(test)]
#[path = "../tests/unit/languages.rs"]
mod tests;
