//! Bash setup hooks (owner script; bash-language-server).
//!
//! Bash is a default language: the server and trace's Node runtime are installed
//! automatically before preflight; without them (automatic installs off) the preflight
//! reports `ServerMissing`. There is no toolchain, no dependency and no build: scripts are
//! never executed, shellcheck and explainshell are disabled.
//!
//! Registry settings that matter (`assets/backends/bash.json`):
//! * `includeAllWorkspaceSymbols: false`: a definition answer comes only from the current
//!   file and the files it sources, which is Bash's own scoping (so an answer is proof);
//! * `ready: log "BackgroundAnalysis: Completed after"` (with `logLevel: info`): the server
//!   analyses nothing until the client answered its `workspace/configuration` request after
//!   `initialized` (documents opened before that are never analysed and every `definition`
//!   on them answers `null`), and the files a script sources are known to it only after its
//!   background analysis read them; that analysis logs this line when it is done (bounded by
//!   the server's own 10 s limit, `ready_timeout_secs` bounds the wait);
//! * `shard: requests`: one file's requests are spread over several processes (each parses
//!   the file once; the server's per-request cost grows with the file size).
//!
//! Extension-less scripts reach this server through the shebang rule
//! (`trace_core::languages::from_shebang`).
//!
//! **External programs** ([`Hooks::external_program`]): a command word the server could not
//! resolve and no script function declares is an external program when a shell of this
//! machine would find it: an executable file in a PATH directory (Windows: with a `PATHEXT`
//! extension), plus on Windows the POSIX tool directories of the bash that runs scripts there
//! (`usr/bin`, `bin`, `mingw64/bin` of the installation `bash` / `git` on PATH belong to, or
//! the standard Git location). The directories are listed once per run; nothing is executed.
//! Their list is part of the fingerprint (another PATH is another answer).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use trace_core::setup_error::SetupError;
use trace_core::Language;
use trace_env::os::{EnvVars, Platform, ProgramDirs};

use super::{default_prepared, Prepared, Server, SetupContext};
use crate::backends::fntype::FnTypeRoute;
use crate::setup::{require_runtimes, require_server, Collect};

pub struct Hooks;

/// Backend-private data of a Bash preflight: where this machine's programs are.
#[derive(Debug)]
pub struct BashData {
    /// Program directories in lookup order (`trace_env::os::shell_program_dirs`).
    pub dirs: Vec<PathBuf>,
    vars: EnvVars,
    platform: Platform,
    /// The directory listings, read on the first lookup.
    programs: OnceLock<ProgramDirs>,
}

impl BashData {
    pub fn new(dirs: Vec<PathBuf>, vars: &EnvVars, platform: &Platform) -> BashData {
        BashData {
            dirs,
            vars: vars.clone(),
            platform: platform.clone(),
            programs: OnceLock::new(),
        }
    }

    /// Whether `command` is an executable program of this machine (never executed).
    pub fn is_program(&self, command: &str) -> bool {
        self.programs
            .get_or_init(|| ProgramDirs::read(&self.dirs, &self.vars, &self.platform))
            .find(command)
            .is_some()
    }
}

fn bash_data(prepared: &Prepared) -> Option<&BashData> {
    prepared.data.as_deref()?.downcast_ref::<BashData>()
}

/// The prepared data of a Bash run on this machine (program directories from `vars`).
pub fn bash_prepared(mut prepared: Prepared, server: &str, vars: &EnvVars, platform: &Platform) -> Prepared {
    let data = BashData::new(trace_env::os::shell_program_dirs(vars, platform), vars, platform);
    let mut fp = trace_core::fingerprint::PartsHasher::new();
    fp.text(server);
    for d in &data.dirs {
        fp.text(&d.display().to_string());
    }
    prepared.fingerprint = fp.finish().hex_prefix(32);
    prepared.data = Some(Arc::new(data));
    prepared
}

impl Server for Hooks {
    fn preflight(&self, cx: &SetupContext<'_>) -> Result<Prepared, SetupError> {
        let mut collect = Collect::default();
        collect.check(require_server(cx));
        collect.check(require_runtimes(cx));
        let server = format!("{} {}", cx.entry.server.name, cx.entry.server.version);
        let prepared = bash_prepared(default_prepared(cx), &server, cx.vars, cx.platform);
        collect.finish(prepared)
    }

    fn fn_type_route(&self, _language: Language) -> FnTypeRoute {
        FnTypeRoute::TableOnly
    }

    /// A command found as a program of this machine (module docs); never executed.
    fn external_program(&self, command: &str, prepared: &Prepared) -> bool {
        bash_data(prepared).is_some_and(|d| d.is_program(command))
    }

    /// `\. file` / `\source file` (the backslash only bypasses aliases) is the source
    /// command; bash-language-server 5.x only follows `. file` / `source file`, so every
    /// function of a file loaded the escaped way stayed unresolved (nvm's tests: 1,545 calls).
    /// The server gets ` . file`: same length, positions unchanged.
    fn server_text<'s>(&self, language: Language, text: &'s str) -> std::borrow::Cow<'s, str> {
        if language != Language::Bash {
            return std::borrow::Cow::Borrowed(text);
        }
        let escaped = escaped_source_words(text.as_bytes());
        if escaped.is_empty() {
            return std::borrow::Cow::Borrowed(text);
        }
        let mut bytes = text.as_bytes().to_vec();
        for at in escaped {
            bytes[at] = b' ';
        }
        // Only an ASCII byte was replaced by an ASCII byte: still UTF-8.
        std::borrow::Cow::Owned(String::from_utf8(bytes).unwrap_or_else(|_| text.to_string()))
    }
}

/// Byte offsets of the backslash of every `\.` / `\source` word (syntax tree: `word` nodes
/// whose whole text is the escaped source command, in command position or folded into the
/// previous command's arguments by the grammar).
pub fn escaped_source_words(source: &[u8]) -> Vec<usize> {
    let Ok(tree) = trace_syntax::parse_tree(Language::Bash, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "word" {
            let text = source.get(node.start_byte()..node.end_byte()).unwrap_or_default();
            // tree-sitter-bash folds a line that starts with `\.` into the previous command
            // as the word "\n\\.": the leading line break belongs to no word in bash.
            let skip = text.iter().take_while(|b| b.is_ascii_whitespace()).count();
            let word = &text[skip..];
            if word == b"\\." || word == b"\\source" {
                out.push(node.start_byte() + skip);
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
#[path = "../../tests/unit/languages/bash.rs"]
mod tests;
