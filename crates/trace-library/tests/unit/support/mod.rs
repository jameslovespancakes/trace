//! Helpers of the trace-library unit tests: in-memory library files, the native leaf and
//! irreducible rows of a test, fixtures, derivation with the default limits and summary
//! lookups.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use trace_core::Language;

use crate::channels::ChannelRow;
use crate::derive::{
    derive_with, DeriveContext, FileSummaries, FunctionSummary, Leaves, ParsedFiles, SourceLoader,
};
use crate::model::Effect;
use crate::table::Section;

/// In-memory library files (never the file system); `MapLoader::default()` has none.
#[derive(Default)]
pub(crate) struct MapLoader(pub(crate) BTreeMap<PathBuf, Vec<u8>>);

impl SourceLoader for MapLoader {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        self.0.get(path).cloned()
    }
}

/// Native leaf effects (by symbol) and irreducible rows of a test; `TestLeaves::default()`
/// has none.
#[derive(Default)]
pub(crate) struct TestLeaves {
    pub(crate) symbols: Vec<(String, Vec<Effect>)>,
    pub(crate) rows: Vec<(Section, ChannelRow)>,
}

impl TestLeaves {
    /// Only irreducible rows.
    pub(crate) fn rows(rows: Vec<(Section, ChannelRow)>) -> TestLeaves {
        TestLeaves {
            rows,
            ..TestLeaves::default()
        }
    }
}

impl Leaves for TestLeaves {
    fn by_symbol(&self, _language: Language, symbol: &str, _positional: u32) -> Vec<Effect> {
        self.symbols
            .iter()
            .filter(|(s, _)| s == symbol)
            .flat_map(|(_, e)| e.iter().cloned())
            .collect()
    }

    fn by_spelling(
        &self,
        _language: Language,
        _qualifier: Option<&str>,
        _name: &str,
        _positional: u32,
    ) -> Vec<Effect> {
        Vec::new()
    }

    fn rows(&self, _language: Language, section: Section) -> Vec<ChannelRow> {
        self.rows
            .iter()
            .filter(|(s, _)| *s == section)
            .map(|(_, r)| r.clone())
            .collect()
    }
}

/// The repository's shared fixtures (`tests/fixtures`).
pub(crate) fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// Bytes of a fixture file (`tests/fixtures/<rel>`).
pub(crate) fn fixture_bytes(rel: &str) -> Vec<u8> {
    let path = fixtures().join(rel);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()))
}

/// Derive `source` as the library file `path` with the default limits.
pub(crate) fn derive_in(
    language: Language,
    path: &Path,
    source: &[u8],
    leaves: &dyn Leaves,
    loader: &dyn SourceLoader,
    roots: &[PathBuf],
) -> FileSummaries {
    let limits = trace_core::config::DeriveSettings::default();
    let cx = DeriveContext {
        leaves,
        loader,
        parsed: &ParsedFiles::default(),
        roots,
        limits: &limits,
    };
    derive_with(language, path, source, &cx)
}

/// The summary whose in-file qualified name is `qualified`.
pub(crate) fn summary<'a>(s: &'a FileSummaries, qualified: &str) -> &'a FunctionSummary {
    s.by_qualified(qualified).unwrap_or_else(|| {
        panic!(
            "no summary for {qualified}; have {:?}",
            s.functions.values().map(|f| &f.qualified).collect::<Vec<_>>()
        )
    })
}

/// A summary whose qualified name ends with `suffix` (module prefixes differ per grammar).
pub(crate) fn summary_ending<'a>(s: &'a FileSummaries, suffix: &str) -> &'a FunctionSummary {
    s.functions
        .values()
        .find(|f| f.qualified.ends_with(suffix))
        .unwrap_or_else(|| {
            panic!(
                "no summary ending in {suffix}; have {:?}",
                s.functions.values().map(|f| &f.qualified).collect::<Vec<_>>()
            )
        })
}

/// Bytes of the compiled test assembly kept as hex text.
pub(crate) fn fixture_assembly() -> Vec<u8> {
    let text =
        String::from_utf8(fixture_bytes("rule-derive-clr-metadata/Lib.Web.dll.hex")).expect("hex text");
    let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).expect("ascii"), 16).expect("hex"))
        .collect()
}
