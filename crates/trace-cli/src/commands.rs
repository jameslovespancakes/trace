//! The one source of command documentation: the top-level help text ([`HELP`], printed
//! verbatim by `trace --help`) and the per-command clap `about` / `long_about` / argument
//! help ([`COMMANDS`], [`GLOBALS`]).

/// `trace --help` (SPEC.md §13, verbatim).
pub const HELP: &str = "trace: what code uses, what it uses, and how it connects (read-only)

  trace show <symbol>...       its exact code, first to last line
  trace symbols [query]        discover canonical declarations without loading source
      --file <text> --tests     restrict to file path or test code (case/helper hints)
      --mode auto|name|body     test-aware discovery, names only, or body identifiers
      --limit <1..100> --offset <n>   page through discovery results
  trace search <literal>      bounded source search, including strings and anonymous scopes
      --file <text> --limit <1..100> --offset <n>   filter / page matching lines
  trace source <file>          exact indexed file-line window (not a whole definition)
      --start <line> --lines <1..200>   bounded source; follow next_line to continue
  trace context <symbol>       its code, who calls it and when, what it calls, tests
  trace uses <symbol>          every use, the entry points it affects, the tests that matter
  trace deps <symbol>          what it runs, and under which condition
  trace path <from> <to>       how one reaches the other, and what is passed along
      --deep                   everything in full: all callers, levels, paths, unconfirmed links

  trace index [--watch]        build / keep the index fresh (queries do this automatically)
      --allow-build            let this project's build tools run (trusted projects only)
      --env <path>             use the dependencies installed at <path>
  trace status                 index health, setup of every language, what is missing
      --install <lang|all|default> [--yes]   install language servers (the default
                               languages' servers install automatically on first use)

symbol:  file:Name.method | file:line | Name.method | name   (a unique name is enough)
marks:   no mark = proven   ~ = inferred   ? = possible
offline: TRACE_OFFLINE=1 (no automatic installs)
";

/// One argument or flag.
#[derive(Clone, Copy, Debug)]
pub struct ArgDoc {
    /// clap id (`deep`, `install`, `symbol`).
    pub name: &'static str,
    pub help: &'static str,
}

/// One command.
#[derive(Clone, Copy, Debug)]
pub struct CommandDoc {
    pub name: &'static str,
    /// clap `about` (one line).
    pub summary: &'static str,
    /// Second paragraph of clap `long_about`.
    pub when_to_use: &'static str,
    pub args: &'static [ArgDoc],
}

const fn arg(name: &'static str, help: &'static str) -> ArgDoc {
    ArgDoc { name, help }
}

const SYMBOL_HELP: &str = "Symbol: file:Name.method, file:line, Name.method or name (`::` and `.` both work; a unique name is enough), or an exact id";
const DEEP_HELP: &str = "Everything in full, unconfirmed links (tier possible, marked `?`) included";

/// Global options (clap `global = true`: accepted before or after the command).
pub const GLOBALS: &[ArgDoc] = &[
    arg("root", "Repository root to inspect (default: current directory)"),
    arg("json", "Print the JSON report (\"schema\": 1) with full provenance instead of text"),
];

pub const COMMANDS: &[CommandDoc] = &[
    CommandDoc {
        name: "search",
        summary: "Find literal source evidence, including strings and anonymous scopes",
        when_to_use: "Use when symbols cannot discover an assertion, anonymous callback, or other source text. Literal and case-sensitive, one result per matching line, in file/line order. Results are bounded previews, not full definitions. Truncation and pagination are explicit; keep the same query and filters with next_offset. Use source for exact surrounding lines, or show for complete named definitions. Only admitted hash-verified indexed files are searched.",
        args: &[arg("query", "Nonempty single-line literal, not a regex or CLI flags"), arg("file", "Restrict to indexed paths containing this text"), arg("limit", "Maximum matching lines per page, 1 to 100"), arg("offset", "Matching lines to skip; use next_offset")],
    },
    CommandDoc {
        name: "source",
        summary: "Read an explicit, bounded source window with absolute lines",
        when_to_use: "Use the exact repo-relative file from search. Returns up to the requested line count, shortened only at EOF; next_line continues. Oversized windows are rejected, never silently truncated. This is an excerpt, not a declaration; show remains complete. Source hashes and indexed-path safety are checked before reading.",
        args: &[arg("file", "Exact indexed repo-relative file path"), arg("start", "First absolute file line, starting at 1"), arg("lines", "Requested lines, 1 to 200; at most 48000 source bytes")],
    },
    CommandDoc {
        name: "symbols",
        summary: "Find indexed declarations without loading their source",
        when_to_use: "Use batched show directly with unambiguous bare or qualified names; full file paths are optional. Auto prefers exact identifiers. Conceptual --tests searches retain broader doc/body matches and rank Python test-case candidates ahead of nested helpers. Role labels are naming/scope heuristics, not verified collection or coverage. --mode name searches names only; --mode body searches indexed body identifiers, not string literals, docs or data initializers. --mode name requires all query words within one name: it is not a batch of names. If discovery is empty, use search for a literal or source for exact lines. Lists definitions without a query. Repeat the same query, mode and filters with the returned offset for the next page.",
        args: &[
            arg("query", "Name, documentation or identifier search; omit to list declarations"),
            arg("file", "Restrict to paths containing this text"),
            arg("tests", "Test code; auto search ranks Python case candidates ahead of helpers"),
            arg("mode", "auto: identifier-first/test-aware; name: names only; body: indexed body identifiers only"),
            arg("limit", "Maximum matches per page, 1 to 100"),
            arg("offset", "Number of ranked matches to skip"),
        ],
    },
    CommandDoc {
        name: "show",
        summary: "The exact code of symbols, first to last line",
        when_to_use: "Call show directly with known unambiguous bare names or qualified names, even without knowing their file paths; batch independent requests. Canonical file-qualified IDs are returned. Ambiguity returns candidates without guessing; no discovery preflight is needed. Each declaration is complete under its identity and source span. Sources have absolute line numbers and batches preserve independent successes. Python module assignments and Go package bindings are non-callable data definitions; syntax-discovered test blocks are retrieval-only candidates. Call counts are not applicable to either. Imports and usages are not canonical definitions.",
        args: &[arg("symbols", "Symbols: file:Name.method, file:line, Name.method or name (a unique name is enough)")],
    },
    CommandDoc {
        name: "context",
        summary: "A symbol's code, who calls it and when, what it calls, and the tests",
        when_to_use: "The symbol's exact code, then one line per neighbour: every caller with the condition it runs under and the values it passes, every call inside with its condition and target, the tests that switch it on, and the commands worth running next. `--deep` adds callers of callers and every level below.",
        args: &[arg("symbol", SYMBOL_HELP), arg("deep", DEEP_HELP)],
    },
    CommandDoc {
        name: "uses",
        summary: "Every place that uses a symbol, what it affects, and the tests that matter",
        when_to_use: "Each use with its caller, exact line and the conditions it runs under; the entry points above the callers; tests that set an option the uses depend on, then tests mentioning it; unresolved same-name sites last under `check:`. `--deep` lists every caller of callers and every test. `--json` adds the evidence of every link.",
        args: &[arg("symbol", SYMBOL_HELP), arg("deep", DEEP_HELP)],
    },
    CommandDoc {
        name: "deps",
        summary: "What a symbol runs, and under which condition",
        when_to_use: "One row per call site in source order: the call on one line, the condition it runs under (shared conditions printed once above their calls) and its target; then the next level (`then`). Unresolved calls are marked `?`. `--deep` adds every level and the unresolved calls below.",
        args: &[arg("symbol", SYMBOL_HELP), arg("deep", DEEP_HELP)],
    },
    CommandDoc {
        name: "path",
        summary: "How one symbol reaches another, and what is passed along",
        when_to_use: "One shortest path, one hop per call: the call on one line, when it runs, and the values it carries into the next function (`argument → parameter`); a cross-language hop prints the bridge it crosses. `--deep` lists every path up to 10.",
        args: &[
            arg("from", "Start symbol (same selector grammar as every command)"),
            arg("to", "Target symbol"),
            arg("deep", DEEP_HELP),
        ],
    },
    CommandDoc {
        name: "index",
        summary: "Build / keep the index fresh (queries do this automatically)",
        when_to_use: "Incremental: only changed files are re-analyzed. Every language of the repository is analyzed by its language server; when a server, toolchain, the project's dependencies or build approval is missing, trace stops with one error that lists everything missing and how to fix it (there is no syntax-only mode). With `--watch`, trace keeps running in the foreground (like a dev server): the language servers stay warm, every file change is applied within about a second and one line is printed per update; other trace commands wait for it instead of answering from an outdated graph. Ctrl+C stops it.",
        args: &[
            arg("watch", "Keep the index fresh while files change, in the foreground (Ctrl+C to stop)"),
            arg(
                "allow_build",
                "Allow this project's build tools (Gradle, Maven, Cargo build scripts, MSBuild, CMake, sbt, cabal, ...) to run for analysis, offline and writing only into trace's cache; remembered for this repository. Only for projects you trust",
            ),
            arg(
                "env",
                "Use the dependencies installed at PATH (a virtual environment, node_modules, ...) instead of the ones found next to the project; remembered for this repository (repeatable)",
            ),
        ],
    },
    CommandDoc {
        name: "status",
        summary: "Index health, setup of every language, what is missing",
        when_to_use: "Freshness, call resolution, one setup row per language (language server, toolchain, dependencies, build approval: ready or what is missing), languages not analyzed yet, whether `trace index --watch` runs; never modifies anything unless `--install` is given. The language servers of the default languages (Python, JavaScript, TypeScript, Java, C#, C, C++, Go, Rust, Bash) install automatically the first time trace needs them (or all at once: `trace status --install default`); every other language installs on request: `trace status --install <lang>`.",
        args: &[
            arg(
                "install",
                "Download and install the pinned language server of <LANG> (a language, `all` = every language of this repository, or `default`) and the runtime it needs into trace's tools folder (this flag is the consent)",
            ),
            arg(
                "yes",
                "accept the licence of the installed language server without a question (scripts)",
            ),
        ],
    },
];

/// The documentation of `name` (panics on an unknown name: a programming error caught by
/// tests).
pub fn doc(name: &str) -> &'static CommandDoc {
    COMMANDS
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("undocumented command {name}"))
}

/// clap `about`.
pub fn summary(name: &str) -> &'static str {
    doc(name).summary
}

/// clap `long_about`.
pub fn long_about(name: &str) -> String {
    let d = doc(name);
    format!("{}.\n\n{}", d.summary, d.when_to_use)
}

/// Help of a command argument.
pub fn arg_help(command: &str, arg: &str) -> &'static str {
    doc(command)
        .args
        .iter()
        .find(|a| a.name == arg)
        .map(|a| a.help)
        .unwrap_or_else(|| panic!("undocumented argument {command} {arg}"))
}

/// Help of a global option.
pub fn global_help(arg: &str) -> &'static str {
    GLOBALS
        .iter()
        .find(|a| a.name == arg)
        .map(|a| a.help)
        .unwrap_or_else(|| panic!("undocumented global option {arg}"))
}

#[cfg(test)]
#[path = "../tests/unit/commands.rs"]
mod tests;
