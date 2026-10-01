//! Declared service contracts: gRPC (`.proto` services; generator naming rules per
//! language) and GraphQL (schema root fields; resolver conventions). Inferred when the
//! contract declares the method/field and exactly one implementation matches, else
//! possible (also when no contract in the repository declares it).

use std::collections::HashMap;

use trace_core::facts::BoundaryRole;
use trace_core::model::{BridgeKind, FileId, Provider, Resolution, SymbolId, Tier};
use trace_core::Language;

use crate::contracts::{GraphqlSchema, ProtoService};
use crate::ctx::{lower_first, parent_dir, Ctx, End};

fn split_service(name: &str) -> Option<(&str, &str)> {
    name.split_once('/')
}

/// Methods implementing a `<Service>/*` server fact: children of the servicer / base-class
/// implementation, or Go methods whose receiver type is the implementation struct (same
/// package directory).
fn server_methods(ctx: &Ctx<'_>, file: FileId, decl: Option<u32>, impl_type: Option<&str>) -> Vec<SymbolId> {
    let mut out = Vec::new();
    let mut types: Vec<String> = impl_type.map(|t| vec![t.to_string()]).unwrap_or_default();
    if let Some(sym) = decl.and_then(|d| ctx.decl_symbol(file, d)) {
        // The servicer class and its subclasses in the same directory (an abstract base
        // servicer whose concrete subclasses implement the rpcs).
        let dir = parent_dir(ctx.path(file));
        let language = ctx.language(file);
        let mut family = vec![sym];
        let mut i = 0;
        while i < family.len() && family.len() < 32 {
            let name = ctx.index.symbol(family[i]).name.clone();
            for s in &ctx.index.symbols {
                if !s.kind.is_type()
                    || family.contains(&s.id)
                    || s.language != language
                    || parent_dir(ctx.path(s.file)) != dir
                {
                    continue;
                }
                let extends = s.bases.iter().any(|b| {
                    let b = b.split(['(', '<', '[']).next().unwrap_or(b).trim();
                    b.rsplit(['.', ':']).next() == Some(name.as_str())
                });
                if extends {
                    family.push(s.id);
                }
            }
            i += 1;
        }
        for k in &family {
            if let Some(kids) = ctx.children.get(k) {
                out.extend(kids.iter().copied());
            }
        }
        if ctx.language(file) == Language::Go {
            types.push(ctx.index.symbol(sym).name.clone());
        }
    }
    let dir = parent_dir(ctx.path(file));
    for t in types {
        if let Some(v) = ctx.by_container.get(t.as_str()) {
            out.extend(
                v.iter()
                    .copied()
                    .filter(|s| parent_dir(ctx.path(ctx.index.symbol(*s).file)) == dir),
            );
        }
    }
    out.retain(|s| {
        let sym = ctx.index.symbol(*s);
        sym.kind.is_callable() && !sym.is_synthetic()
    });
    out.sort();
    out.dedup();
    out
}

pub(crate) fn grpc(ctx: &mut Ctx<'_>, services: &[ProtoService]) {
    let mut servers: HashMap<(String, String), Vec<End>> = HashMap::new();
    for f in ctx.of(BridgeKind::Grpc, BoundaryRole::Provides) {
        let Some((service, method)) = split_service(f.name()) else { continue };
        if method == "*" {
            for m in server_methods(ctx, f.file, f.fact.decl, f.detail("impl_type")) {
                let name = ctx.index.symbol(m).name.clone();
                let end = ctx.symbol_end(m);
                servers
                    .entry((service.to_string(), lower_first(&name)))
                    .or_default()
                    .push(end);
            }
        } else {
            // A handler named in the registration table (`charge: Server.chargeHandler.bind(this)`)
            // is the implementation; the registering scope only stands in when it cannot be resolved.
            let handler = f
                .fact
                .decl
                .and_then(|d| ctx.decl_symbol(f.file, d))
                .or_else(|| {
                    f.detail("handler")
                        .and_then(|h| crate::http::resolve_handler_text(ctx, f.file, h))
                })
                .or_else(|| {
                    f.detail("handler_span")
                        .and_then(crate::http::span_of)
                        .and_then(|s| ctx.edge_target_in(f.file, s))
                        .filter(|s| ctx.index.symbol(*s).kind.is_callable())
                });
            let end = match handler {
                Some(sym) => Some(End { sym, at: f.at() }),
                None => ctx.end_of(&f),
            };
            if let Some(end) = end {
                servers
                    .entry((service.to_string(), lower_first(method)))
                    .or_default()
                    .push(end);
            }
        }
    }
    if servers.is_empty() {
        return;
    }
    let mut declared: HashMap<(String, String), Vec<(&ProtoService, &str)>> = HashMap::new();
    for s in services {
        for rpc in &s.rpcs {
            declared
                .entry((s.name.clone(), lower_first(rpc)))
                .or_default()
                .push((s, rpc.as_str()));
        }
    }
    for u in ctx.of(BridgeKind::Grpc, BoundaryRole::Uses) {
        let Some((service, method)) = split_service(u.name()) else { continue };
        let key = (service.to_string(), lower_first(method));
        let Some(cands) = servers.get(&key).cloned() else { continue };
        let Some(from) = ctx.end_of(&u) else { continue };
        let decl = declared.get(&key);
        // Copies of one contract (same package, service and rpc: the same fully qualified
        // gRPC method `/pkg.Service/Rpc` on the wire) declare one method, not several.
        let same_method = decl.is_some_and(|v| {
            v.len() > 1
                && v.iter()
                    .all(|(s, rpc)| s.package == v[0].0.package && s.name == v[0].0.name && *rpc == v[0].1)
        });
        let (label, contract, assumption) = match decl.map(Vec::as_slice) {
            Some([(s, rpc)]) => {
                let pkg = if s.package.is_empty() {
                    String::new()
                } else {
                    format!("{}.", s.package)
                };
                (
                    format!("{pkg}{}/{rpc}", s.name),
                    Some(s.file),
                    "the .proto service declares this rpc; generated stub and server naming rules link both sides".to_string(),
                )
            }
            Some(many) if same_method => {
                let (s, rpc) = many[0];
                let pkg = if s.package.is_empty() {
                    String::new()
                } else {
                    format!("{}.", s.package)
                };
                (
                    format!("{pkg}{}/{rpc}", s.name),
                    Some(s.file),
                    format!(
                        "{} .proto copies declare the same method {pkg}{}/{rpc}; generated stub and server naming rules link both sides",
                        many.len(),
                        s.name
                    ),
                )
            }
            Some(many) if !many.is_empty() => (
                format!("{service}/{method}"),
                None,
                format!("{} .proto services named {service} declare this rpc", many.len()),
            ),
            _ => (
                format!("{service}/{method}"),
                None,
                "no .proto in the repository declares this rpc; matched by generator naming rules only"
                    .to_string(),
            ),
        };
        let tier = if contract.is_some() {
            Tier::Inferred
        } else {
            Tier::Possible
        };
        ctx.emit(
            BridgeKind::Grpc,
            from,
            &cands,
            tier,
            Provider::Contract("proto".into()),
            Resolution::ContractMatch,
            &label,
            &[assumption],
            contract,
        );
    }
}

pub(crate) fn graphql(ctx: &mut Ctx<'_>, schema: &GraphqlSchema) {
    let canonical = |t: &str| -> String { schema.roots.get(t).cloned().unwrap_or_else(|| t.to_string()) };
    let mut resolvers: HashMap<(String, String), Vec<End>> = HashMap::new();
    for f in ctx.of(BridgeKind::Graphql, BoundaryRole::Provides) {
        let Some((ty, field)) = f.name().split_once('.') else { continue };
        let Some(end) = ctx.end_of(&f) else { continue };
        resolvers
            .entry((canonical(ty), field.to_string()))
            .or_default()
            .push(end);
    }
    if resolvers.is_empty() {
        return;
    }
    // Declared root fields by canonical root type.
    let mut declared: HashMap<(String, String), Vec<FileId>> = HashMap::new();
    for ((ty, field), files) in &schema.fields {
        declared
            .entry((canonical(ty), field.clone()))
            .or_default()
            .extend(files.iter().copied());
    }
    for u in ctx.of(BridgeKind::Graphql, BoundaryRole::Uses) {
        let Some((ty, field)) = u.name().split_once('.') else { continue };
        let key = (ty.to_string(), field.to_string());
        let Some(cands) = resolvers.get(&key).cloned() else { continue };
        let Some(from) = ctx.end_of(&u) else { continue };
        let files = declared.get(&key).cloned().unwrap_or_default();
        let (contract, assumption) = match files.as_slice() {
            [one] => (Some(*one), "the GraphQL schema declares this root field; the resolver implements it"),
            [] => (None, "no GraphQL schema in the repository declares this field; matched by resolver conventions only"),
            _ => (None, "several schema files declare this field"),
        };
        ctx.emit(
            BridgeKind::Graphql,
            from,
            &cands,
            if contract.is_some() {
                Tier::Inferred
            } else {
                Tier::Possible
            },
            Provider::Contract("graphql".into()),
            Resolution::ContractMatch,
            u.name(),
            &[assumption.to_string()],
            contract,
        );
    }
}
