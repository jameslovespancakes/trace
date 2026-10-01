//! `status` text: index health, languages, setup, resolution, library behaviour, bridges,
//! settings and install hints.

use std::fmt::Write as _;

use trace_analysis::report::{
    LanguageRow, LibraryBehaviourStatus, PendingRow, ResolutionHealth, SettingRow, StatusReport,
};
use trace_core::SupportLevel;
use trace_semantic::setup::SetupRow;

use super::count;

/// What the status text needs from a [`StatusReport`].
pub(super) struct StatusView<'a> {
    pub(super) exists: bool,
    pub(super) fresh: Option<bool>,
    pub(super) stale_files: usize,
    pub(super) files: usize,
    pub(super) symbols: usize,
    pub(super) pending_files: usize,
    /// Dependents left stale by an interface change (updated before the next answer).
    pub(super) stale: usize,
    pub(super) outside_build_files: usize,
    pub(super) request_failed_files: usize,
    pub(super) languages: &'a [LanguageRow],
    pub(super) resolution: &'a [ResolutionHealth],
    pub(super) setup: &'a [SetupRow],
    pub(super) pending: &'a [PendingRow],
    pub(super) default_install: &'a [String],
    pub(super) build_approval: &'a str,
    pub(super) env_paths: Vec<(String, String)>,
    pub(super) excluded: &'a [String],
    pub(super) settings: &'a [SettingRow],
    pub(super) watching: bool,
    pub(super) library: &'a LibraryBehaviourStatus,
    pub(super) cache: &'a str,
    pub(super) install: Option<&'a trace_analysis::install::InstallReport>,
}

impl<'a> StatusView<'a> {
    pub(super) fn of(r: &'a StatusReport) -> Self {
        StatusView {
            exists: r.index.exists,
            fresh: r.index.fresh,
            stale_files: r.index.stale_files.len(),
            files: r.index.files,
            symbols: r.index.symbols,
            pending_files: r.index.pending_files,
            stale: r.index.stale,
            outside_build_files: r.index.outside_build_files,
            request_failed_files: r.index.request_failed_files,
            languages: &r.languages,
            resolution: &r.resolution,
            setup: &r.setup,
            pending: &r.pending,
            default_install: &r.default_install,
            build_approval: r.build_approval,
            env_paths: r.env_paths.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            excluded: &r.excluded,
            settings: &r.settings,
            watching: r.watching,
            library: &r.library_behaviour,
            cache: &r.cache,
            install: r.install.as_ref(),
        }
    }
}

/// `1,724`.
pub(super) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `semantic (pyright 1.1.414) · 83 files` (reason omitted when empty).
pub(super) fn language_text(support: SupportLevel, reason: &str, files: u32) -> String {
    let mut s = support.as_str().to_string();
    if !reason.is_empty() {
        let _ = write!(s, " ({reason})");
    }
    let _ = write!(s, " \u{b7} {}", count(files as usize, "file", "files"));
    s
}

/// `96%` (ties to even, like the golden files).
fn percent(n: usize, total: usize) -> String {
    format!("{:.0}%", (100.0 * n as f64 / total as f64).round_ties_even())
}

/// Rate of in-repository calls first, all calls in parentheses (SPEC 11 status):
/// `(in-repo, all)` texts, `None` without call sites.
fn rates<'r>(rows: impl Iterator<Item = &'r ResolutionHealth> + Clone) -> Option<(Option<String>, String)> {
    let resolved: usize = rows.clone().map(|r| r.resolved).sum();
    let total = resolved + rows.clone().map(|r| r.unresolved).sum::<usize>();
    if total == 0 {
        return None;
    }
    let in_resolved: usize = rows.clone().map(|r| r.in_repo_resolved).sum();
    let in_total = in_resolved + rows.map(|r| r.in_repo_unresolved).sum::<usize>();
    let in_repo = (in_total > 0).then(|| percent(in_resolved, in_total));
    Some((in_repo, percent(resolved, total)))
}

/// `ready · jdtls 1.61.0 · jdk 21.0.4 (C:\jdk) · dependencies installed (2 folders) · build
/// allowed` or `error: <one-line setup error>` of one setup row.
fn setup_text(row: &SetupRow) -> String {
    if let Some(error) = &row.error {
        return format!("error: {error}");
    }
    let mut parts: Vec<String> = vec![row.status.to_string()];
    if let Some(server) = &row.server {
        parts.push(server.clone());
    }
    if let Some(toolchain) = &row.toolchain {
        parts.push(toolchain.clone());
    }
    if let Some(deps) = &row.dependencies {
        parts.push(format!("dependencies {deps}"));
    }
    parts.push(format!("build {}", row.build));
    parts.extend(row.notes.iter().cloned());
    parts.join(" \u{b7} ")
}

/// `yes` / `no - run trace index --watch ...` (PLAN decision 13).
fn watching_text(watching: bool) -> &'static str {
    if watching {
        "yes"
    } else {
        "no - run trace index --watch to keep the graph fresh while you work"
    }
}

pub(super) fn status_text(v: &StatusView<'_>) -> String {
    let mut rows: Vec<(String, String)> = Vec::new();
    let index = if !v.exists {
        "not indexed \u{2014} run: trace index".to_string()
    } else {
        let state = if v.fresh == Some(true) {
            "fresh".to_string()
        } else {
            format!("stale ({} changed) \u{2014} run: trace index", count(v.stale_files, "file", "files"))
        };
        let mut parts = vec![
            state,
            count(v.files, "file", "files"),
            format!("{} symbols", thousands(v.symbols)),
        ];
        match rates(v.resolution.iter()) {
            Some((Some(in_repo), all)) => parts.push(format!("{in_repo} in-repo calls resolved ({all} all)")),
            Some((None, all)) => parts.push(format!("{all} calls resolved")),
            None => {}
        }
        if v.pending_files > 0 {
            parts.push(format!("{} not analyzed yet", count(v.pending_files, "file", "files")));
        }
        if v.outside_build_files > 0 {
            parts.push(format!("{} outside the build here", count(v.outside_build_files, "file", "files")));
        }
        if v.request_failed_files > 0 {
            parts.push(format!(
                "{} with failed server requests",
                count(v.request_failed_files, "file", "files")
            ));
        }
        parts.join(" \u{b7} ")
    };
    rows.push(("index".to_string(), index));
    if v.stale > 0 {
        rows.push((
            "stale".to_string(),
            format!("{} (updated before the next answer)", count(v.stale, "file", "files")),
        ));
    }
    let mut shown: Vec<trace_core::Language> = Vec::new();
    for l in v.languages {
        shown.push(l.language);
        let setup = v.setup.iter().find(|s| s.language == l.language);
        let mut what = match (l.support, setup) {
            (SupportLevel::Pending, _) | (SupportLevel::Inventoried, _) | (_, None) => {
                language_text(l.support, &l.reason, l.files)
            }
            (_, Some(s)) => format!("{} \u{b7} {}", setup_text(s), count(l.files as usize, "file", "files")),
        };
        let lang_rows = v.resolution.iter().filter(|r| r.language == l.language);
        match rates(lang_rows.clone()) {
            Some((Some(in_repo), all)) => {
                let _ = write!(what, " \u{b7} {in_repo} in-repo ({all} all)");
            }
            Some((None, all)) => {
                let _ = write!(what, " \u{b7} {all} calls resolved");
            }
            None => {}
        }
        // Calls resolved only because no repository declaration carries their name (I-07).
        let by_name: usize = lang_rows.clone().map(|r| r.by_name).sum();
        if by_name > 0 {
            let _ = write!(what, " ({by_name} by name)");
        }
        if lang_rows.clone().any(|r| r.warning.is_some()) {
            what.push_str(" \u{b7} low resolution");
        }
        let server = lang_rows.clone().find_map(|r| r.server).map(|s| match s {
            "server_missing" => "server missing",
            "server_not_ready" => "server not ready",
            "server_failed" => "server failed",
            other => other,
        });
        if let Some(server) = server {
            let _ = write!(what, " \u{b7} {server}");
        }
        rows.push((l.language.as_str().to_string(), what));
    }
    // Setup rows of languages the index does not have yet (no index, or new files).
    for s in v.setup.iter().filter(|s| !shown.contains(&s.language)) {
        rows.push((s.language.as_str().to_string(), setup_text(s)));
    }
    for p in v.pending {
        rows.push((
            "pending".to_string(),
            format!("{}: {} ({})", p.language.display_name(), count(p.files, "file", "files"), p.reason),
        ));
    }
    for line in v.default_install {
        rows.push(("install".to_string(), line.clone()));
    }
    rows.push((
        "build approval".to_string(),
        if v.build_approval == "allowed" {
            "allowed (trace index --allow-build)".to_string()
        } else {
            "not given".to_string()
        },
    ));
    for (ecosystem, path) in &v.env_paths {
        rows.push(("env".to_string(), format!("{ecosystem} {path}")));
    }
    if !v.excluded.is_empty() {
        rows.push(("excluded".to_string(), v.excluded.join(", ")));
    }
    for s in v.settings {
        rows.push(("setting".to_string(), format!("{} = {} \u{b7} {}", s.key, s.value, s.origin)));
    }
    if v.library.sites > 0 {
        let known = v.library.derived + v.library.declared_type + v.library.table;
        rows.push((
            "library".to_string(),
            format!(
                "{known} of {} functions passed to libraries have known behaviour ({}; {} derived, {} declared types, {} table)",
                v.library.sites,
                percent(known, v.library.sites),
                v.library.derived,
                v.library.declared_type,
                v.library.table
            ),
        ));
    }
    rows.push(("watching".to_string(), watching_text(v.watching).to_string()));
    rows.push(("cache".to_string(), v.cache.to_string()));
    if let Some(r) = v.install {
        for line in trace_analysis::install::report_text(r).lines() {
            rows.push(("install".to_string(), line.to_string()));
        }
    }
    let w = rows.iter().map(|(a, _)| a.chars().count()).max().unwrap_or(0);
    rows.iter()
        .map(|(a, b)| format!("{a:<w$}  {b}").trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

// ------------------------------------------------------------------ index
