use super::*;
use trace_core::model::{ByteSpan, EdgeKind, FileId, Resolution, SymbolId, Tier};

fn edge(from: u32, to: u32, file: u32, provider: Provider) -> Edge {
    Edge {
        from: SymbolId(from),
        to: SymbolId(to),
        kind: EdgeKind::Calls,
        tier: Tier::Proven,
        provider,
        resolution: Resolution::InheritanceRule,
        at: Location {
            file: FileId(file),
            bytes: ByteSpan::new(1, 2),
            line: 1,
        },
        site: None,
        bridge: None,
    }
}

#[test]
fn rule_previous_rule_edges_are_remapped_or_dropped() {
    let remap = IdRemap {
        files: vec![Some(FileId(1)), None],
        symbols: vec![Some(SymbolId(5)), Some(SymbolId(6)), None],
    };
    let kept = remap_edges(&[edge(0, 1, 0, Provider::Rule("python-mro".into()))], &remap);
    assert_eq!(kept.len(), 1);
    assert_eq!((kept[0].from, kept[0].to, kept[0].at.file), (SymbolId(5), SymbolId(6), FileId(1)));
    assert!(remap_edges(&[edge(0, 2, 0, Provider::Pyright)], &remap).is_empty(), "removed symbol");
    assert!(remap_edges(&[edge(0, 1, 1, Provider::Pyright)], &remap).is_empty(), "removed file");
    assert!(is_import_rule(&edge(0, 1, 0, Provider::Rule(trace_infer::imports::RULE.into()))));
    assert!(!is_import_rule(&edge(0, 1, 0, Provider::Rule("python-mro".into()))));
}

/// The library phase (derived summaries, standard-library index) is part of the total the
/// profile's `total` line and the `indexed ... Ns` line print, and `apply` records it.
#[test]
fn rule_index_total_includes_the_library_phase() {
    let secs = PhaseSeconds {
        semantic: 2.0,
        library: 77.0,
        ..PhaseSeconds::default()
    };
    assert!((secs.total() - 79.0).abs() < 1e-9);
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/pipeline/update.rs"));
    let body = &source[source.find("let knowledge = {").unwrap()..];
    let bridges_at = body.find("// Bridges (after library").unwrap();
    assert!(body[..bridges_at].contains("secs.library = t.elapsed()"));
}

#[test]
fn rule_phase_state_round_trips_by_version() {
    let knowledge = LibraryKnowledge::default();
    let state = state_of(LIBRARY_PHASE, 3, &knowledge).unwrap();
    let states = vec![state];
    assert_eq!(take_state::<LibraryKnowledge>(&states, LIBRARY_PHASE, 3), Some(knowledge));
    assert_eq!(take_state::<LibraryKnowledge>(&states, LIBRARY_PHASE, 4), None, "other version");
    assert_eq!(take_state::<LibraryKnowledge>(&states, SITES_PHASE, 3), None);
}
