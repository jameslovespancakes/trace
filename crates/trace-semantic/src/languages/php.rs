//! PHP setup hooks (owner script; Intelephense).
//!
//! Preflight (every independent failure collected, PLAN decision 15):
//! 1. server: Intelephense (installed on request after its licence was accepted,
//!    `trace status --install php [--yes]`) and trace's Node runtime;
//! 2. toolchain: PHP is optional; when installed its version refines `phpVersion`;
//! 3. dependencies: Composer `require` (+ `require-dev` after a dev install) against
//!    `<vendor>/composer/installed.json` -> `DepsMissing` ("composer install"); `--env` may point
//!    to a vendor directory;
//! 4. no build approval: Composer and PHP never run.
//!
//! Server inputs (`Prepared.json_vars`, expanded by the generic launcher in `php.json`):
//! * `php_environment` = `{"includePaths": [<vendor dir>], "phpVersion": "<x.y.z>"}`: the
//!   installed vendor tree is indexed read-only from its place (it is excluded from trace's
//!   inventory, so it is not in the snapshot). Measured on guzzle: definitions 54% -> 96.5%.
//! * `php_stubs` = Intelephense's default stub list plus the `ext-*` extensions the project
//!   requires that the installed server ships stubs for (the setting replaces the default
//!   list, so the full list is sent).
//!
//! Index storage lives in the backend's state dir (`{outside}`, `clearCache: false`), so the
//! vendor index is reused across runs.
//!
//! Sub-projects: every other directory with a `composer.json` (e.g. per-tool installs under
//! `vendor-bin/<tool>`) is listed in the status notes, installed or not; their files are
//! analysed with the root project's vendor tree.
//!
//! Edits (warm sessions, [`Server::settle_changes`]): Intelephense takes a changed file
//! into its workspace index asynchronously; the document answers (`documentSymbol`) can be
//! current while cross-file definitions still come from the old index. After every update
//! the hook waits (bounded) until the workspace index agrees with the changed files: for the
//! last declarations of each changed file (an edit moves what follows it) the syntax tree
//! gives the name line, `documentSymbol` must place the name there, and every
//! `workspace/symbol` entry of that name in that file must start where `documentSymbol` puts
//! the declaration. A server that never agrees is the timeout error, never a stale answer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trace_core::model::SymbolKind;
use trace_core::setup_error::{timeout_minutes, SetupError};
use trace_core::text::LineIndex;
use trace_core::Language;
use trace_env::{EcosystemId, ToolchainStatus};

use super::{default_prepared, detect_context, read_context, Prepared, Server, SettleContext, SetupContext};
use crate::backends::fntype::FnTypeRoute;
use crate::lsp::{capability_enabled, uri_to_path};
use crate::setup::{deps_error, require_runtimes, require_server, Collect};
use crate::SemanticError;

/// Changed files probed per update ([`Hooks::settle_changes`]).
const SETTLE_FILES: usize = 8;
/// Declarations probed per changed file: the last ones (an edit moves what follows it).
const SETTLE_PROBES: usize = 4;
/// Longest wait for the workspace index to take the changes in.
const SETTLE_LIMIT: Duration = Duration::from_secs(60);
/// Pause between two probe rounds.
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// Intelephense 1.18.5's default `intelephense.stubs` list (server configuration mirrored
/// from the pinned server's own default; the setting replaces it when sent).
pub const DEFAULT_STUBS: &[&str] = &[
    "apache",
    "bcmath",
    "bz2",
    "calendar",
    "com_dotnet",
    "Core",
    "ctype",
    "curl",
    "date",
    "dba",
    "dom",
    "enchant",
    "exif",
    "fileinfo",
    "filter",
    "fpm",
    "ftp",
    "gd",
    "hash",
    "iconv",
    "imap",
    "intl",
    "json",
    "ldap",
    "libxml",
    "mbstring",
    "mcrypt",
    "mssql",
    "mysqli",
    "oci8",
    "odbc",
    "openssl",
    "pcntl",
    "pcre",
    "PDO",
    "pgsql",
    "Phar",
    "posix",
    "pspell",
    "random",
    "readline",
    "Reflection",
    "regex",
    "session",
    "shmop",
    "SimpleXML",
    "snmp",
    "soap",
    "sockets",
    "sodium",
    "SPL",
    "sqlite3",
    "standard",
    "superglobals",
    "sybase",
    "sysvmsg",
    "sysvsem",
    "sysvshm",
    "tidy",
    "tokenizer",
    "uri",
    "xml",
    "xmlreader",
    "xmlrpc",
    "xmlwriter",
    "Zend OPcache",
    "zip",
    "zlib",
];

pub struct Hooks;

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let language = Language::Php;
        let mut collect = Collect::default();
        collect.check(require_server(cx));
        collect.check(require_runtimes(cx));

        // vendor/ and composer files are only READ (inside the repository too): only the
        // user's protected roots are off limits. The PHP toolchain may be executed (`php -v`),
        // so its search keeps `cx.tools.forbidden_roots` (the repository).
        let readable = trace_core::paths::forbidden_roots();
        let exec = detect_context(cx, EcosystemId::Php);
        let dcx = read_context(cx, EcosystemId::Php, &readable);
        let toolchain = match trace_env::php::toolchain(&exec) {
            ToolchainStatus::Found(t) => Some(t),
            _ => None,
        };
        let setup = trace_env::php::setup(&dcx, toolchain.as_ref());
        match &setup.env_not_found {
            Some(path) => collect.push(SetupError::EnvNotFound {
                language: Some(language),
                path: path.clone(),
            }),
            None => {
                if let Some(e) = deps_error(language, &setup.deps) {
                    collect.push(e);
                }
            }
        }

        let stub_dir = cx
            .tools
            .tool_dir("intelephense")
            .map(|d| d.join("node_modules").join("intelephense").join("lib").join("stub"));
        let stubs = stubs(&setup.extensions, stub_dir.as_deref());
        let include_paths: Vec<String> = if setup.vendor_installed {
            vec![setup.vendor_dir.display().to_string()]
        } else {
            Vec::new()
        };
        let mut prepared = default_prepared(cx);
        prepared.json_vars.insert(
            "php_environment".into(),
            json!({"includePaths": include_paths, "phpVersion": setup.php_version}),
        );
        prepared.json_vars.insert("php_stubs".into(), json!(stubs));
        prepared.library_roots = setup.deps.roots.clone();
        prepared.status = setup.deps.notes.clone();
        prepared
            .status
            .push(format!("PHP version for the server: {}", setup.php_version));
        if setup.vendor_installed {
            prepared
                .status
                .push(format!("vendor: {}", setup.vendor_dir.display()));
        }
        prepared
            .status
            .extend(subproject_notes(&cx.repo.root, &setup.deps.subprojects));
        prepared.toolchain = toolchain;
        let mut h = blake3::Hasher::new();
        h.update(setup.deps.fingerprint.as_bytes());
        h.update(
            serde_json::Value::Object(prepared.json_vars.clone().into_iter().collect())
                .to_string()
                .as_bytes(),
        );
        prepared.fingerprint = h.finalize().to_hex()[..32].to_string();
        collect.finish(prepared)
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::Label
    }

    /// Wait until Intelephense's workspace index took the changed files in (module docs).
    fn settle_changes(&self, cx: &mut SettleContext<'_>) -> Result<(), SetupError> {
        let log = cx.client.log_path().to_path_buf();
        // Indexing the server announced (`indexingStarted` / progress) ends first.
        cx.client
            .settle_after_changes()
            .map_err(|e| settle_failure(e, &log))?;
        let capabilities = cx.client.capabilities();
        if !capability_enabled(capabilities, "documentSymbolProvider")
            || !capability_enabled(capabilities, "workspaceSymbolProvider")
        {
            return Ok(());
        }
        let probes = changed_probes(cx.changed);
        if probes.is_empty() {
            return Ok(());
        }
        let client = &mut *cx.client;
        let deadline = Instant::now() + SETTLE_LIMIT;
        let mut ask = |calls: Vec<(String, Value)>| client.request_many(calls);
        wait_index_current(&probes, &mut ask, deadline, SETTLE_POLL).map_err(|e| settle_failure(e, &log))
    }
}

/// One status line per Composer sub-project: its directory, and whether its own vendor tree
/// is installed (`vendor/composer/installed.json` next to its `composer.json`).
pub fn subproject_notes(root: &Path, subprojects: &[trace_env::SubProject]) -> Vec<String> {
    subprojects
        .iter()
        .map(|sp| {
            let dir: PathBuf = sp.dir.split('/').fold(root.to_path_buf(), |p, s| p.join(s));
            let installed = dir.join("vendor").join("composer").join("installed.json").is_file();
            let state = if installed {
                "its dependencies are installed"
            } else {
                "its dependencies are not installed (composer install in that folder)"
            };
            format!(
                "sub-project {}: {}; {state}; its files are analyzed with the root's dependencies",
                sp.dir, sp.reason
            )
        })
        .collect()
}

/// A settle failure as the catalogue error: a request timeout / the deadline is the timeout
/// error, a broken connection the crash error.
fn settle_failure(e: SemanticError, log: &Path) -> SetupError {
    match e {
        SemanticError::Setup(s) => s,
        SemanticError::Timeout { .. } | SemanticError::Deadline => SetupError::ServerTimeout {
            language: Language::Php,
            minutes: timeout_minutes(SETTLE_LIMIT),
            log: log.to_path_buf(),
        },
        _ => SetupError::ServerCrashed {
            language: Language::Php,
            log: log.to_path_buf(),
        },
    }
}

/// One changed file and the declarations probed in it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Probe {
    uri: String,
    file: PathBuf,
    decls: Vec<ProbeDecl>,
}

/// A declaration of a changed file: its name and the 0-based line of the name (syntax tree).
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProbeDecl {
    name: String,
    line: u32,
}

/// Probes of the changed PHP files (at most [`SETTLE_FILES`]; unreadable / deleted files and
/// files without a probed declaration are skipped).
fn changed_probes(changed: &[String]) -> Vec<Probe> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for uri in changed {
        if out.len() >= SETTLE_FILES {
            break;
        }
        if !seen.insert(uri.as_str()) {
            continue;
        }
        let Ok(file) = uri_to_path(uri) else { continue };
        if trace_core::languages::from_path(&file) != Some(Language::Php) {
            continue;
        }
        let Ok(source) = std::fs::read(&file) else { continue };
        let decls = probe_decls(&file.to_string_lossy(), &source);
        if !decls.is_empty() {
            out.push(Probe {
                uri: uri.clone(),
                file,
                decls,
            });
        }
    }
    out
}

/// The last [`SETTLE_PROBES`] functions / methods of a PHP file whose name is unique in it,
/// with the 0-based line of the name, in source order.
fn probe_decls(path: &str, source: &[u8]) -> Vec<ProbeDecl> {
    let Ok(facts) = trace_syntax::extract(trace_syntax::SourceInput {
        path,
        language: Language::Php,
        source,
    }) else {
        return Vec::new();
    };
    let callable = |kind: SymbolKind| {
        matches!(kind, SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor)
    };
    let named = |name: &str| name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for d in &facts.declarations {
        if callable(d.kind) {
            *counts.entry(d.name.as_str()).or_default() += 1;
        }
    }
    let lines = LineIndex::new(source);
    let mut decls: Vec<(u32, ProbeDecl)> = facts
        .declarations
        .iter()
        .filter(|d| callable(d.kind) && named(&d.name) && counts.get(d.name.as_str()) == Some(&1))
        .map(|d| {
            (
                d.name_span.start,
                ProbeDecl {
                    name: d.name.clone(),
                    line: lines.line0(d.name_span.start),
                },
            )
        })
        .collect();
    decls.sort_by_key(|(start, _)| *start);
    let skip = decls.len().saturating_sub(SETTLE_PROBES);
    decls.into_iter().skip(skip).map(|(_, d)| d).collect()
}

/// The requests of one probe round: per file its `documentSymbol`, then one
/// `workspace/symbol` per probed declaration.
fn probe_calls(probes: &[Probe]) -> Vec<(String, Value)> {
    let mut calls = Vec::new();
    for p in probes {
        calls.push(("textDocument/documentSymbol".to_string(), json!({"textDocument": {"uri": p.uri}})));
        for d in &p.decls {
            calls.push(("workspace/symbol".to_string(), json!({"query": d.name})));
        }
    }
    calls
}

/// One probe round: the requests -> their answers (or the round's error).
type ProbeAsk<'a> =
    dyn FnMut(Vec<(String, Value)>) -> Result<Vec<Result<Value, SemanticError>>, SemanticError> + 'a;

/// Ask probe rounds until the workspace index agrees with every changed file
/// ([`index_current`]), pausing `poll` between rounds; past `deadline` it is
/// [`SemanticError::Deadline`].
fn wait_index_current(
    probes: &[Probe],
    ask: &mut ProbeAsk<'_>,
    deadline: Instant,
    poll: Duration,
) -> Result<(), SemanticError> {
    loop {
        let answers = ask(probe_calls(probes))?;
        if index_current(probes, &answers) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(SemanticError::Deadline);
        }
        std::thread::sleep(poll);
    }
}

/// A symbol name of a server answer matches a declared name: equal, or qualified by a
/// container (`Class::name`, `Class->name`, `Ns\name`, `Class.name`), with an optional
/// parameter list (`name()`).
fn same_name(server: &str, name: &str) -> bool {
    let bare = server.split_once('(').map_or(server, |(head, _)| head).trim();
    bare == name
        || ["::", "->", "\\", "."]
            .iter()
            .any(|sep| bare.strip_suffix(name).is_some_and(|head| head.ends_with(*sep)))
}

/// Whether a `file:` URI names `file` (case-insensitively on Windows).
fn same_file(uri: &str, file: &Path) -> bool {
    let Ok(path) = uri_to_path(uri) else { return false };
    if cfg!(windows) {
        path.to_string_lossy().replace('\\', "/").to_lowercase()
            == file.to_string_lossy().replace('\\', "/").to_lowercase()
    } else {
        path == file
    }
}

/// One `documentSymbol` entry: name, 0-based start / end line of its range, and the start
/// line of its selection (name) range when the answer is hierarchical.
struct DocSymbol {
    name: String,
    start: u32,
    end: u32,
    selection: Option<u32>,
}

fn line_of(v: &Value, key: &str) -> Option<u32> {
    v.get(key)?.get("line")?.as_u64().and_then(|l| u32::try_from(l).ok())
}

/// Every entry of a `documentSymbol` answer (hierarchical `DocumentSymbol` with children, or
/// flat `SymbolInformation`).
fn doc_symbols(answer: &Value) -> Vec<DocSymbol> {
    let mut out = Vec::new();
    let mut stack: Vec<&Value> = answer.as_array().map(|a| a.iter().collect()).unwrap_or_default();
    while let Some(item) = stack.pop() {
        if out.len() > 100_000 {
            break;
        }
        let Some(name) = item.get("name").and_then(Value::as_str) else { continue };
        let range = item
            .get("range")
            .or_else(|| item.get("location").and_then(|l| l.get("range")));
        let Some(range) = range else { continue };
        let (Some(start), Some(end)) = (line_of(range, "start"), line_of(range, "end")) else {
            continue;
        };
        let selection = item.get("selectionRange").and_then(|r| line_of(r, "start"));
        out.push(DocSymbol {
            name: name.to_string(),
            start,
            end,
            selection,
        });
        if let Some(children) = item.get("children").and_then(Value::as_array) {
            stack.extend(children.iter());
        }
    }
    out
}

/// Whether one probe round shows the workspace index current for every changed file:
/// * the document answer is current: an entry named like the declaration puts its name on
///   the syntax line (selection range; a flat answer's range must contain it) - a file the
///   server does not list the name for gives no evidence;
/// * the index agrees: every `workspace/symbol` entry of that name in that file starts on a
///   line where the document answer has the declaration (range or selection start).
///
/// Failed requests give no evidence either way.
fn index_current(probes: &[Probe], answers: &[Result<Value, SemanticError>]) -> bool {
    let mut next = answers.iter();
    for probe in probes {
        let Some(doc) = next.next() else { return false };
        let found: Vec<Option<&Value>> = probe
            .decls
            .iter()
            .map(|_| next.next().and_then(|r| r.as_ref().ok()))
            .collect();
        let Ok(doc) = doc else { continue };
        let entries = doc_symbols(doc);
        for (decl, answer) in probe.decls.iter().zip(found) {
            let named: Vec<&DocSymbol> = entries.iter().filter(|e| same_name(&e.name, &decl.name)).collect();
            if named.is_empty() {
                continue;
            }
            let document_current = named.iter().any(|e| match e.selection {
                Some(line) => line == decl.line,
                None => e.start <= decl.line && decl.line <= e.end,
            });
            if !document_current {
                return false;
            }
            let lines: BTreeSet<u32> = named
                .iter()
                .flat_map(|e| [Some(e.start), e.selection])
                .flatten()
                .collect();
            let Some(items) = answer.and_then(Value::as_array) else { continue };
            for item in items {
                let Some(name) = item.get("name").and_then(Value::as_str) else { continue };
                let Some(location) = item.get("location") else { continue };
                let Some(uri) = location.get("uri").and_then(Value::as_str) else { continue };
                if !same_name(name, &decl.name) || !same_file(uri, &probe.file) {
                    continue;
                }
                let Some(start) = location.get("range").and_then(|r| line_of(r, "start")) else {
                    continue;
                };
                if !lines.contains(&start) {
                    return false;
                }
            }
        }
    }
    true
}

/// The default stubs plus the required extensions the installed server has stubs for
/// (matched case-insensitively against `lib/stub/<name>`), in a stable order.
pub fn stubs(extensions: &[String], stub_dir: Option<&Path>) -> Vec<String> {
    let available: Vec<String> = stub_dir
        .and_then(|d| std::fs::read_dir(d).ok())
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    let mut out: Vec<String> = DEFAULT_STUBS.iter().map(|s| s.to_string()).collect();
    let mut seen: BTreeSet<String> = out.iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut extra: Vec<String> = extensions
        .iter()
        .filter_map(|ext| {
            let lower = ext.to_ascii_lowercase();
            available.iter().find(|a| a.to_ascii_lowercase() == lower).cloned()
        })
        .filter(|name| seen.insert(name.to_ascii_lowercase()))
        .collect();
    extra.sort();
    out.extend(extra);
    out
}

#[cfg(test)]
#[path = "../../tests/unit/languages/php.rs"]
mod tests;
