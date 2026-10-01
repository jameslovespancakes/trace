use super::*;
use crate::languages::Language;
use crate::model::*;
use crate::test_support::index::{decision, index_with, site};

/// Materialized edges produced by site `site` (inferred and possible), in edge order.
fn site_edges<'g>(g: &'g Graph, site: u32) -> impl Iterator<Item = (EdgeId, &'g Edge)> + 'g {
    let owner = g.index.sites.get(site as usize).map(|s| s.owner);
    owner
        .into_iter()
        .flat_map(move |o| g.outgoing_all(o))
        .filter(move |(_, e)| e.site == Some(site))
}

#[test]
fn tiers_materialize_separately() {
    let mut index = index_with(&["main", "a", "b", "c"], &[(0, 1)]);
    index.sites.push(site(1, &[2, 3]));
    index.decisions.push(decision(0, &[2]));
    index.validate().unwrap();
    let g = Graph::new(&index);
    assert_eq!(
        g.counts(),
        TierCounts {
            proven: 1,
            inferred: 1,
            possible: 1
        }
    );
    let proven: Vec<_> = g.outgoing(SymbolId(1), Tier::Proven).collect();
    assert!(proven.is_empty());
    let inferred: Vec<_> = g.outgoing(SymbolId(1), Tier::Inferred).collect();
    assert_eq!(inferred.len(), 1);
    assert_eq!(inferred[0].1.kind, EdgeKind::InferredCall);
    assert_eq!(inferred[0].1.provider, Provider::Deterministic);
    assert_eq!(inferred[0].1.resolution, Resolution::DeterministicUnique);
    let possible: Vec<_> = g.outgoing(SymbolId(1), Tier::Possible).collect();
    assert_eq!(possible.len(), 2);
    assert!(possible
        .iter()
        .any(|(_, e)| e.kind == EdgeKind::PossibleLink && e.to == SymbolId(3)));
    assert_eq!(site_edges(&g, 0).count(), 2);
    assert_eq!(g.incoming(SymbolId(1), Tier::Proven).count(), 1);
}

/// I-01: a dispatch site whose receiver type is proven (receiver-type rule) materializes
/// its one implementation as a proven edge of the activation kind; the other candidates
/// are not linked. Without the proven receiver the same site stays inferred / possible.
#[test]
fn rule_dispatch_with_proven_receiver_type_is_proven() {
    let mut index = index_with(&["main", "Sink.matched", "A.matched", "B.matched"], &[(0, 1)]);
    let mut s = site(0, &[2, 3]);
    s.category = SiteCategory::Dispatch;
    s.declared_target = Some(SymbolId(1));
    s.receiver_exact = true;
    s.flow_candidates = vec![SymbolId(2)];
    index.sites.push(s);
    index.decisions.push(decision(0, &[2]));
    index.validate().unwrap();
    assert!(proven_by_receiver_rule(&index.sites[0], &index.decisions[0]));
    let g = Graph::new(&index);
    let proven: Vec<_> = g
        .outgoing(SymbolId(0), Tier::Proven)
        .filter(|(_, e)| e.site == Some(0))
        .map(|(_, e)| e.clone())
        .collect();
    assert_eq!(proven.len(), 1);
    assert_eq!(proven[0].to, SymbolId(2));
    assert_eq!(proven[0].kind, EdgeKind::Calls);
    assert_eq!(proven[0].provider, Provider::Rule(RECEIVER_TYPE_RULE.into()));
    assert!(site_edges(&g, 0).all(|(_, e)| e.to != SymbolId(3)));

    // Without the proven receiver: inferred.
    index.sites[0].receiver_exact = false;
    index.decisions[0] = decision(0, &[2]);
    let g = Graph::new(&index);
    let kinds: Vec<EdgeKind> = site_edges(&g, 0).map(|(_, e)| e.kind).collect();
    assert_eq!(kinds, vec![EdgeKind::InferredDispatch, EdgeKind::PossibleLink]);
}

#[test]
fn decisions_cannot_escape_candidates() {
    let mut index = index_with(&["main", "a", "b", "c"], &[]);
    index.sites.push(site(0, &[1]));
    // A decision naming a non-candidate never becomes an edge.
    index.decisions.push(decision(0, &[3]));
    index.validate().unwrap();
    let g = Graph::new(&index);
    assert!(g.edges().iter().all(|e| e.to != SymbolId(3)));
    assert_eq!(g.counts().inferred, 0);
    assert_eq!(g.counts().possible, 1);
}

#[test]
fn bridges_keep_their_tier_and_can_be_disabled() {
    let mut index = index_with(&["client", "handler"], &[]);
    let loc = |line: u32| Location {
        file: FileId(0),
        bytes: ByteSpan::new(line, line + 1),
        line,
    };
    index.bridges.push(Bridge {
        kind: BridgeKind::Http,
        tier: Tier::Inferred,
        from: SymbolId(0),
        to: SymbolId(1),
        from_at: loc(1),
        to_at: loc(2),
        provider: Provider::Contract("http".into()),
        resolution: Resolution::RouteMatch,
        label: "POST /auth/login".into(),
        assumptions: Vec::new(),
        candidates: 1,
        contract: None,
    });
    index.validate().unwrap();
    let mut g = Graph::new(&index);
    assert_eq!(g.outgoing(SymbolId(0), Tier::Proven).count(), 0);
    let e: Vec<_> = g.outgoing(SymbolId(0), Tier::Inferred).collect();
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].1.kind, e[0].1.tier, e[0].1.bridge), (EdgeKind::Bridge, Tier::Inferred, Some(0)));
    g.set_bridges(false);
    assert!(!g.bridges_enabled());
    assert_eq!(g.outgoing(SymbolId(0), Tier::Possible).count(), 0);
    assert_eq!(BridgeKind::Http.edge_label(), "bridge:http");
    assert_eq!("bridge:pyo3".parse::<BridgeKind>(), Ok(BridgeKind::Pyo3));

    let mut bad = index.clone();
    bad.bridges[0].kind = BridgeKind::Subprocess; // weak boundaries are possible only
    assert!(bad.validate().is_err());
}

#[test]
fn family_closure_is_transitive_both_ways_and_bounded() {
    // 0 base, 1 and 2 override 0, 3 overrides 1, 4 implements 5 (another family), 6 alone.
    let mut index = index_with(&["Base.m", "A.m", "B.m", "C.m", "D.m", "I.m", "other"], &[]);
    let fam = |from: u32, to: u32, kind: EdgeKind| Edge {
        from: SymbolId(from),
        to: SymbolId(to),
        kind,
        tier: Tier::Proven,
        provider: Provider::Rule("python-mro".into()),
        resolution: Resolution::InheritanceRule,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(from * 10 + 4, from * 10 + 5),
            line: from + 1,
        },
        site: None,
        bridge: None,
    };
    index.edges.push(fam(1, 0, EdgeKind::Overrides));
    index.edges.push(fam(2, 0, EdgeKind::Overrides));
    index.edges.push(fam(3, 1, EdgeKind::Overrides));
    index.edges.push(fam(4, 5, EdgeKind::Implements));
    index.validate().unwrap();
    let (members, cut) = family_closure(&index, SymbolId(3), MAX_FAMILY);
    assert_eq!(members, vec![SymbolId(3), SymbolId(1), SymbolId(0), SymbolId(2)]);
    assert!(!cut);
    let (members, cut) = family_closure(&index, SymbolId(3), 2);
    assert_eq!(members.len(), 2);
    assert!(cut);
    assert_eq!(Graph::new(&index).family(SymbolId(5)).0, vec![SymbolId(5), SymbolId(4)]);
    assert_eq!(family_closure(&index, SymbolId(6), MAX_FAMILY).0, vec![SymbolId(6)]);
    // Family kinds are never traversed as execution.
    assert_eq!(Graph::new(&index).outgoing(SymbolId(1), Tier::Possible).count(), 0);
}

/// Symbols for family tests: `(qualified, kind, language, parent, container, stub)`.
type Spec<'s> = (&'s str, SymbolKind, Language, Option<u32>, Option<&'s str>, bool);

fn family_index(specs: &[Spec<'_>]) -> Index {
    let names: Vec<&str> = specs.iter().map(|s| s.0).collect();
    let mut index = index_with(&names, &[]);
    for (i, &(_, kind, language, parent, container, stub)) in specs.iter().enumerate() {
        let s = &mut index.symbols[i];
        s.kind = kind;
        s.language = language;
        s.parent = parent.map(SymbolId);
        s.container = container.map(str::to_string);
        s.is_stub = stub;
    }
    index
}

fn family_edge(from: u32, to: u32, kind: EdgeKind) -> Edge {
    Edge {
        from: SymbolId(from),
        to: SymbolId(to),
        kind,
        tier: Tier::Proven,
        provider: Provider::Rule("java-inheritance".into()),
        resolution: Resolution::InheritanceRule,
        at: Location {
            file: FileId(0),
            bytes: ByteSpan::new(from * 10 + 4, from * 10 + 5),
            line: from + 1,
        },
        site: None,
        bridge: None,
    }
}

fn relations(family: &Family) -> Vec<(u32, &'static str)> {
    let mut out: Vec<(u32, &'static str)> =
        family.members.iter().map(|m| (m.id.0, m.relation.as_str())).collect();
    out.sort_unstable();
    out
}

/// Java / C# overloads and Python property getter + setter pairs (same name, same
/// declaring type) are one family; a C prototype joins its definition as a declaration;
/// overrides join transitively; other names of the same type never do.
#[test]
fn rule_target_family_includes_overloads_and_declarations() {
    use SymbolKind::{Class, Function, Method};
    let index = family_index(&[
        ("Json", Class, Language::Java, None, None, false),
        ("Json.parse", Method, Language::Java, Some(0), None, false),
        ("Json.parse", Method, Language::Java, Some(0), None, false),
        ("Json.stringify", Method, Language::Java, Some(0), None, false),
        ("JToken", Class, Language::CSharp, None, None, false),
        ("JToken.DeepClone", Method, Language::CSharp, Some(4), None, false),
        ("JToken.DeepClone", Method, Language::CSharp, Some(4), None, false),
        ("Box", Class, Language::Python, None, None, false),
        ("Box.value", Method, Language::Python, Some(7), None, false),
        ("Box.value", Method, Language::Python, Some(7), None, false),
        ("area", Function, Language::C, None, None, true),
        ("area", Function, Language::C, None, None, false),
        ("Sub", Class, Language::Java, None, None, false),
        ("Sub.parse", Method, Language::Java, Some(12), None, false),
        // Free functions of one name are never overloads of each other.
        ("helper", Function, Language::Java, None, None, false),
        ("helper", Function, Language::Java, None, None, false),
    ]);
    let mut index = index;
    index.edges.push(family_edge(10, 11, EdgeKind::StubImplementation));
    index.edges.push(family_edge(13, 1, EdgeKind::Overrides));

    let java = target_family(&index, SymbolId(1), MAX_FAMILY);
    assert_eq!(relations(&java), vec![(1, "target"), (2, "overload"), (13, "override")]);
    assert!(!java.truncated);
    assert_eq!(java.members[0].id, SymbolId(1));
    assert_eq!(java.relation(SymbolId(2)), Some(FamilyRelation::Overload));
    assert!(!java.contains(SymbolId(3)));
    // The override is reached from the overload group too (transitively).
    let from_overload = target_family(&index, SymbolId(2), MAX_FAMILY);
    assert_eq!(relations(&from_overload), vec![(1, "overload"), (2, "target"), (13, "override")]);

    let csharp = target_family(&index, SymbolId(6), MAX_FAMILY);
    assert_eq!(relations(&csharp), vec![(5, "overload"), (6, "target")]);

    let property = target_family(&index, SymbolId(8), MAX_FAMILY);
    assert_eq!(relations(&property), vec![(8, "target"), (9, "overload")]);

    let definition = target_family(&index, SymbolId(11), MAX_FAMILY);
    assert_eq!(relations(&definition), vec![(10, "declaration"), (11, "target")]);
    let prototype = target_family(&index, SymbolId(10), MAX_FAMILY);
    assert_eq!(prototype.relation(SymbolId(11)), Some(FamilyRelation::Declaration));

    assert_eq!(target_family(&index, SymbolId(14), MAX_FAMILY).ids(), vec![SymbolId(14)]);

    let cut = target_family(&index, SymbolId(1), 2);
    assert_eq!(cut.members.len(), 2);
    assert!(cut.truncated);
}

/// Rust: an inherent `impl Printer { fn matched }` and `impl Sink for Printer { fn
/// matched }` are different functions (only the trait method belongs to the trait's
/// family); Go methods are not overloads either.
#[test]
fn rule_rust_trait_and_inherent_methods_are_not_overloads() {
    use SymbolKind::{Class, Interface, Method};
    let mut index = family_index(&[
        ("Sink", Interface, Language::Rust, None, None, false),
        ("Sink.matched", Method, Language::Rust, Some(0), None, true),
        ("Printer", Class, Language::Rust, None, None, false),
        ("Printer.matched", Method, Language::Rust, None, Some("Printer"), false),
        ("Printer.matched", Method, Language::Rust, None, Some("Printer"), false),
        ("Server", Class, Language::Go, None, None, false),
        ("Server.Run", Method, Language::Go, None, Some("Server"), false),
        ("Server.Run", Method, Language::Go, None, Some("Server"), false),
    ]);
    index.edges.push(family_edge(4, 1, EdgeKind::Implements));

    let inherent = target_family(&index, SymbolId(3), MAX_FAMILY);
    assert_eq!(inherent.ids(), vec![SymbolId(3)]);
    let trait_impl = target_family(&index, SymbolId(4), MAX_FAMILY);
    assert_eq!(relations(&trait_impl), vec![(1, "implements"), (4, "target")]);
    let trait_method = target_family(&index, SymbolId(1), MAX_FAMILY);
    assert_eq!(relations(&trait_method), vec![(1, "target"), (4, "implements")]);
    assert!(!trait_method.contains(SymbolId(3)));

    assert_eq!(target_family(&index, SymbolId(6), MAX_FAMILY).ids(), vec![SymbolId(6)]);
}

#[test]
fn deterministic_decisions_are_labelled() {
    let mut index = index_with(&["main", "a"], &[]);
    index.sites.push(site(0, &[1]));
    index.decisions.push(decision(0, &[1]));
    let g = Graph::new(&index);
    let e = &g.edges()[0];
    assert_eq!(e.provider, Provider::Deterministic);
    assert_eq!(e.resolution, Resolution::DeterministicUnique);
    assert_eq!(e.tier, Tier::Inferred);
    assert_eq!(g.symbol_by_uid("a.py:a"), Some(SymbolId(1)));
}
