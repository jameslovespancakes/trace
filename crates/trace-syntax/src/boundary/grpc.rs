//! gRPC stub calls (generated client code named by the `rpc_*` convention rows).

use trace_core::facts::BoundaryRole;
use trace_core::model::BridgeKind;
use trace_core::Language;

use super::{Bind, CallView, Cx};
use crate::node::{named_children, span};

impl<'a> Cx<'a> {
    pub(super) fn grpc_call(&self, cv: &CallView<'_>) {
        // Stub calls: `stub.SayHello(req)`, and calls on a stub created inline by the
        // generated factory (`pb.NewGreeterClient(conn).SayHello(ctx, req)`).
        let stub = match self.bind_of_receiver(cv) {
            Some((_, Bind::Stub { service })) => Some(service.clone()),
            _ => cv
                .receiver
                .map(|r| self.unwrap(r))
                .and_then(|r| self.call_view(r))
                .and_then(|inner| self.grpc_stub_service(&inner)),
        };
        if let Some(service) = stub {
            let method = cv.member.strip_suffix("Async").unwrap_or(&cv.member);
            if !method.is_empty() && method != "close" && method != "Close" {
                self.push(
                    BridgeKind::Grpc,
                    BoundaryRole::Uses,
                    format!("{service}/{method}"),
                    cv.owner,
                    None,
                    span(cv.node),
                    vec![("service".into(), service.clone()), ("method".into(), method.to_string())],
                );
            }
            return;
        }
        match self.lang {
            Language::Go => {
                // `pb.Register<Service>Server(server, impl)`.
                let Some(svc) = self.service_of("rpc_server_register", &cv.member) else {
                    return;
                };
                if cv.positional_count() < 2 {
                    return;
                }
                let mut detail = vec![
                    ("service".to_string(), svc.clone()),
                    ("rule".to_string(), "go_register".to_string()),
                ];
                if let Some(arg) = cv.positional(1) {
                    let impl_node = crate::node::find_descendant(arg, 8, |n| n.kind() == "composite_literal")
                        .or_else(|| (arg.kind() == "composite_literal").then_some(arg));
                    if let Some(ty) = impl_node.and_then(|c| c.child_by_field_name("type")) {
                        detail.push(("impl_type".into(), crate::node::type_name(ty, self.src)));
                    }
                }
                self.push(
                    BridgeKind::Grpc,
                    BoundaryRole::Provides,
                    format!("{svc}/*"),
                    cv.owner,
                    None,
                    span(cv.node),
                    detail,
                );
            }
            Language::JavaScript | Language::TypeScript | Language::Tsx => {
                // `server.addService(Service.service, {rpc: handler})`.
                if !self.names("rpc_add_service", BridgeKind::Grpc, &cv.member) {
                    return;
                }
                let (Some(svc_node), Some(table)) = (cv.positional(0), cv.positional(1)) else {
                    return;
                };
                let segs: Vec<String> = self
                    .path_of(svc_node)
                    .into_iter()
                    .filter(|s| s != "service" && s != "?" && s != "()")
                    .collect();
                let Some(svc) = segs.last().cloned() else { return };
                let table = self.unwrap(table);
                if table.kind() != "object" {
                    return;
                }
                for entry in named_children(table) {
                    let (key, value) = match entry.kind() {
                        "pair" => (entry.child_by_field_name("key"), entry.child_by_field_name("value")),
                        "shorthand_property_identifier" => (Some(entry), Some(entry)),
                        "method_definition" => (entry.child_by_field_name("name"), Some(entry)),
                        _ => (None, None),
                    };
                    let (Some(k), Some(v)) = (key, value) else { continue };
                    let method = self.key_text(k);
                    let mut detail = vec![
                        ("service".to_string(), svc.clone()),
                        ("rule".to_string(), "node_add_service".to_string()),
                    ];
                    let decl = if entry.kind() == "method_definition" {
                        self.decl_of_node(entry)
                    } else {
                        self.handler_info(v, &mut detail)
                    };
                    self.push(
                        BridgeKind::Grpc,
                        BoundaryRole::Provides,
                        format!("{svc}/{method}"),
                        cv.owner,
                        decl,
                        span(entry),
                        detail,
                    );
                }
            }
            _ => {}
        }
    }
}
