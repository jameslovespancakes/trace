//! Precision gate of derived library behaviour (DESIGN §4.13 task 10; owner derive).
//!
//! ```text
//! derive_gate --labels <labels.json> --library [<language>=]<dir> [--library ...]
//!             [--language <language>] [--json] [--write <assets/library/gate.json>]
//! ```
//!
//! Reads the lab's labelled sample (`codepath-lab/scripts/libderive/labels.json`, schema of
//! `export_labels.py`: per language `known`, `negatives`, `hand_checked`), derives every
//! labelled library function from installed source with the same engine and tables `trace`
//! uses (no cache), and scores each label ([`trace_library::gate::judge`]): a derived fact is
//! a running effect (`calls`, `stored_then_called`, `wraps`, `property`, `partial`) on the
//! labelled argument; correct on a positive label, wrong on a negative one. Native labels
//! (compiled code) belong to the native table and are not derivation facts. Precision =
//! correct / derived facts, per language.
//!
//! `--library <dir>` serves every language, `--library python=<dir>` one language (repeat for
//! more directories; site-packages and the interpreter's `Lib/`, `GOROOT/src`, a package
//! cache, ...). `--write` stores `precision`, `facts`, `sample` and `passed` (facts > 0 and
//! precision >= the file's threshold) for every language with labels; other entries and the
//! bridges block stay unchanged.
//!
//! Locating a label's function:
//! * `module` + `name`: the module's file (Python dotted modules below the directories); a
//!   name the module re-exports is found in the files of its package; `method: true` = a
//!   method of that name of any class in the module's package (effects united);
//! * `api` (library-qualified symbol): the longest module prefix names the file, the rest is
//!   the qualified name in it;
//! * `file` (+ `library` `<package>@<version>`): the file below a directory (or below
//!   `<package>`, `<package>@<version>`, `<package>-<version>` in it), or the entry of a
//!   source archive given as the directory (the JDK's `lib/src.zip`); `api` picks the
//!   function.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use trace_core::facts::ImportKind;
use trace_core::Language;
use trace_library::derive::{
    derive_with, DeriveContext, FileSummaries, FsLoader, FunctionSummary, ParsedFiles, SourceLoader,
};
use trace_library::gate::{effects_at, judge, Label, LabelFile, Score, Verdict};
use trace_library::table::Tables;

/// Python files scanned per package for a re-exported name / a method.
const MAX_PACKAGE_FILES: usize = 400;

struct Args {
    labels: PathBuf,
    /// (language or None = every language, directory)
    libraries: Vec<(Option<Language>, PathBuf)>,
    only: Option<Language>,
    json: bool,
    write: Option<PathBuf>,
}

const USAGE: &str = "Usage: derive_gate --labels <labels.json> --library [<language>=]<dir> [--language <language>] [--json] [--write <gate.json>]";

fn parse_args() -> Result<Args, String> {
    let mut labels = None;
    let mut libraries = Vec::new();
    let mut only = None;
    let mut json = false;
    let mut write = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--labels" => labels = it.next().map(PathBuf::from),
            "--library" => {
                let value = it.next().ok_or(USAGE)?;
                let scoped = value
                    .split_once('=')
                    .and_then(|(l, d)| l.parse::<Language>().ok().map(|l| (Some(l), PathBuf::from(d))));
                libraries.push(scoped.unwrap_or((None, PathBuf::from(value))));
            }
            "--language" => {
                let value = it.next().ok_or(USAGE)?;
                only = Some(
                    value
                        .parse::<Language>()
                        .map_err(|_| format!("Unknown language: {value}"))?,
                );
            }
            "--json" => json = true,
            "--write" => write = it.next().map(PathBuf::from),
            other => return Err(format!("Unknown argument: {other}\n{USAGE}")),
        }
    }
    let labels = labels.ok_or(USAGE)?;
    if libraries.is_empty() {
        return Err("Give at least one library directory: --library <dir>".to_string());
    }
    Ok(Args {
        labels,
        libraries,
        only,
        json,
        write,
    })
}

/// Derivation with the builtin tables, file-system (and archive) loading, cached per file.
struct Deriver<'a> {
    tables: &'a Tables,
    derived: BTreeMap<(Language, PathBuf), FileSummaries>,
}

impl Deriver<'_> {
    fn summaries(&mut self, language: Language, path: &Path, roots: &[PathBuf]) -> &FileSummaries {
        let tables = self.tables;
        self.derived.entry((language, path.to_path_buf())).or_insert_with(|| {
            let bytes = FsLoader.read(path).unwrap_or_default();
            let cx = DeriveContext {
                leaves: tables,
                loader: &FsLoader,
                parsed: &ParsedFiles::default(),
                roots,
                limits: &trace_core::config::current().derive,
            };
            derive_with(language, path, &bytes, &cx)
        })
    }
}

/// `a::b`, `a\b`, `a/b`, `a#b` -> `a.b`.
fn normalize(symbol: &str) -> String {
    symbol.replace("::", ".").replace(['\\', '/', '#'], ".")
}

/// The summary of `api` in `summaries`: the same symbol, else the longest qualified name the
/// api ends with (dot-separated).
fn by_api<'s>(summaries: &'s FileSummaries, api: &str) -> Option<&'s FunctionSummary> {
    let api = normalize(api);
    if let Some(found) = summaries.functions.values().find(|f| normalize(&f.symbol) == api) {
        return Some(found);
    }
    summaries
        .functions
        .values()
        .filter(|f| {
            let q = normalize(&f.qualified);
            !q.is_empty() && (api == q || api.ends_with(&format!(".{q}")))
        })
        .max_by_key(|f| f.qualified.len())
}

/// Python module file of a dotted module name below the directories.
fn python_module(module: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let from = dirs.first()?.join("__derive_gate__.py");
    let resolve = trace_library::languages::adapter(Language::Python)?.resolve_import;
    resolve(&from, module, ImportKind::Module, dirs)
        .filter(|(_, member)| member.is_none())
        .map(|(p, _)| p)
}

/// The longest dotted prefix of `api` that is a Python module, and the rest.
fn python_api(api: &str, dirs: &[PathBuf]) -> Option<(PathBuf, String)> {
    let parts: Vec<&str> = api.split('.').collect();
    for k in (1..parts.len()).rev() {
        if let Some(file) = python_module(&parts[..k].join("."), dirs) {
            return Some((file, parts[k..].join(".")));
        }
    }
    None
}

/// Python files of the package holding `file` (the file first), bounded.
fn package_files(file: &Path) -> Vec<PathBuf> {
    let mut out = vec![file.to_path_buf()];
    if file.file_name().is_none_or(|n| n != "__init__.py") {
        return out;
    }
    let Some(dir) = file.parent() else { return out };
    let mut stack = vec![dir.to_path_buf()];
    let mut found: Vec<PathBuf> = Vec::new();
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() && p.join("__init__.py").is_file() {
                stack.push(p);
            } else if kind.is_file() && p.extension().is_some_and(|x| x == "py") && p != file {
                found.push(p);
            }
        }
        if found.len() > MAX_PACKAGE_FILES {
            break;
        }
    }
    found.sort();
    found.truncate(MAX_PACKAGE_FILES);
    out.extend(found);
    out
}

/// Whether `file` declares a function / class named `name` at the top level (`method`: a
/// method of a class).
fn declares(language: Language, file: &Path, name: &str, method: bool) -> bool {
    let Some(source) = FsLoader.read(file) else { return false };
    let text = file.to_string_lossy();
    let Ok(facts) = trace_syntax::extract(trace_syntax::SourceInput {
        path: &text,
        language,
        source: &source,
    }) else {
        return false;
    };
    facts.declarations.iter().enumerate().any(|(i, d)| {
        facts.module_decl != Some(i as u32)
            && d.name == name
            && match d.parent {
                None => !method,
                Some(p) => {
                    method
                        && facts
                            .declarations
                            .get(p as usize)
                            .is_some_and(|parent| parent.kind.is_type())
                }
            }
    })
}

/// The file of a hand-checked label below the directories.
fn labelled_file(label: &Label, dirs: &[PathBuf]) -> Option<PathBuf> {
    let file = label.file.as_deref()?;
    let (package, version) = match label.library.as_deref().and_then(|l| l.rsplit_once('@')) {
        Some((p, v)) => (Some(p), Some(v)),
        None => (label.library.as_deref(), None),
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        // A source archive given as the directory (`--library java=<jdk>/lib/src.zip`).
        if dir.is_file() {
            let entry = trace_library::archive::entry_path(dir, file);
            if trace_library::archive::exists(&entry) {
                return Some(entry);
            }
            continue;
        }
        candidates.push(dir.join(file));
        if let Some(p) = package {
            candidates.push(dir.join(p).join(file));
            if let Some(v) = version {
                candidates.push(dir.join(format!("{p}@{v}")).join(file));
                candidates.push(dir.join(format!("{p}-{v}")).join(file));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Effects derived on the label's argument (`None`: the function was not found).
fn derived_for(
    language: Language,
    label: &Label,
    dirs: &[PathBuf],
    deriver: &mut Deriver<'_>,
) -> Option<BTreeSet<String>> {
    let arg = label.arg.as_ref();
    let param = label.param.as_deref();
    // 1. `file` + `api`.
    if label.file.is_some() {
        let path = labelled_file(label, dirs)?;
        let summaries = deriver.summaries(language, &path, dirs);
        return by_api(summaries, &label.api).map(|s| effects_at(s, arg, param));
    }
    if language != Language::Python {
        return None;
    }
    // 2. `module` + `name` (+ `method`).
    if let (Some(module), Some(name)) = (label.module.as_deref(), label.name.as_deref()) {
        let file = python_module(module, dirs)?;
        let files = package_files(&file);
        if label.method {
            let mut out: Option<BTreeSet<String>> = None;
            for f in files.iter().filter(|f| declares(language, f, name, true)) {
                let summaries = deriver.summaries(language, f, dirs);
                let suffix = format!(".{name}");
                for s in summaries
                    .functions
                    .values()
                    .filter(|s| s.qualified.ends_with(&suffix))
                {
                    out.get_or_insert_with(BTreeSet::new)
                        .extend(effects_at(s, arg, param));
                }
            }
            return out;
        }
        for f in &files {
            if f != &file && !declares(language, f, name, false) {
                continue;
            }
            let summaries = deriver.summaries(language, f, dirs);
            if let Some(s) = summaries.by_qualified(name) {
                return Some(effects_at(s, arg, param));
            }
        }
        return None;
    }
    // 3. `api`: module prefix + qualified name.
    let (file, qualified) = python_api(&label.api, dirs)?;
    let summaries = deriver.summaries(language, &file, dirs);
    // `Class.__init__` labels name the constructor; the class summary merges it.
    let summary = summaries.by_qualified(&qualified).or_else(|| {
        qualified
            .strip_suffix(".__init__")
            .or_else(|| qualified.strip_suffix(".__new__"))
            .and_then(|class| summaries.by_qualified(class))
    })?;
    Some(effects_at(summary, arg, param))
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::from(2);
        }
    };
    let file = match std::fs::read_to_string(&args.labels)
        .map_err(|e| e.to_string())
        .and_then(|text| LabelFile::parse(&text).map_err(|e| e.to_string()))
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error: Could not read the labels at {}: {e}", args.labels.display());
            return ExitCode::from(2);
        }
    };
    let tables = Tables::builtin();
    let threshold = trace_library::gate::Gate::load_builtin()
        .map(|g| g.threshold)
        .unwrap_or(0.9);
    let started = std::time::Instant::now();
    let mut deriver = Deriver {
        tables: &tables,
        derived: BTreeMap::new(),
    };
    let mut results: BTreeMap<String, (Score, String)> = BTreeMap::new();
    for (name, block) in &file.languages {
        let Ok(language) = name.parse::<Language>() else {
            eprintln!("Error: Unknown language in the labels: {name}");
            continue;
        };
        if args.only.is_some_and(|o| o != language) {
            continue;
        }
        let dirs: Vec<PathBuf> = args
            .libraries
            .iter()
            .filter(|(l, _)| l.is_none_or(|l| l == language))
            .map(|(_, d)| d.clone())
            .collect();
        let mut score = Score::default();
        for (part, label) in file.labels(name) {
            let derived = if label.native {
                None
            } else {
                derived_for(language, label, &dirs, &mut deriver)
            };
            let verdict = judge(label, part, derived.as_ref());
            let note = match (&verdict, &derived) {
                (Verdict::Miss | Verdict::Wrong | Verdict::WrongOther | Verdict::NotCounted, Some(d)) => {
                    format!(": derived {d:?}")
                }
                _ => String::new(),
            };
            score.add(label, part, verdict, &note);
        }
        if score.labels == 0 {
            continue;
        }
        let sample = format!(
            "labels.json schema {} ({} known, {} negatives, {} hand-checked)",
            file.schema,
            block.known.len(),
            block.negatives.len(),
            block.hand_checked.len()
        );
        results.insert(
            trace_library::languages::table_language(language)
                .as_str()
                .to_string(),
            (score, sample),
        );
    }
    let seconds = started.elapsed().as_secs_f64();
    if args.json {
        let report: BTreeMap<&str, serde_json::Value> = results
            .iter()
            .map(|(lang, (score, sample))| {
                (
                    lang.as_str(),
                    serde_json::json!({
                        "sample": sample,
                        "facts": score.facts(),
                        "precision": score.precision(),
                        "threshold": threshold,
                        "passed": score.passed(threshold),
                        "score": score,
                    }),
                )
            })
            .collect();
        let out = serde_json::json!({"languages": report, "files_derived": deriver.derived.len(), "seconds": seconds});
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        for (lang, (score, _)) in &results {
            println!(
                "{lang}: precision {:.3} on {} derived facts ({} correct), {} misses, {} not found, {} native, threshold {:.2} -> {}",
                score.precision(),
                score.facts(),
                score.correct,
                score.misses.len(),
                score.not_found.len(),
                score.native,
                threshold,
                if score.passed(threshold) { "passed" } else { "not passed" }
            );
            for w in &score.wrong {
                println!("  wrong: {w}");
            }
            for w in &score.wrong_other {
                println!("  wrong (not running): {w}");
            }
            for m in &score.misses {
                println!("  miss: {m}");
            }
            for n in &score.not_found {
                println!("  not found: {n}");
            }
        }
    }
    if let Some(gate_path) = &args.write {
        if let Err(e) = write_gate(gate_path, &results, threshold) {
            eprintln!("Error: Could not update {}: {e}", gate_path.display());
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

/// Update the entries of every scored language in gate.json (other entries and the bridges
/// block unchanged).
fn write_gate(
    path: &Path,
    results: &BTreeMap<String, (Score, String)>,
    threshold: f64,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut gate: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let languages = gate
        .get_mut("languages")
        .and_then(|l| l.as_object_mut())
        .ok_or("gate.json has no languages object")?;
    for (lang, (score, sample)) in results {
        languages.insert(
            lang.clone(),
            serde_json::json!({
                "precision": (score.precision() * 1000.0).round() / 1000.0,
                "facts": score.facts(),
                "sample": sample,
                "passed": score.passed(threshold),
            }),
        );
    }
    let out = serde_json::to_string_pretty(&gate).map_err(|e| e.to_string())?;
    std::fs::write(path, out + "\n").map_err(|e| e.to_string())
}
