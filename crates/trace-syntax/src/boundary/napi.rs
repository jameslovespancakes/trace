//! Node-API: `napi_create_function` / property descriptor tables (C / C++) and the
//! JavaScript side of native addons.

use trace_core::facts::BoundaryRole;
use trace_core::model::BridgeKind;
use trace_core::Language;
use tree_sitter::Node;

use super::{
    conventions::last_segment, conventions::symbol_segments, Bind, CallView, Cx, NAPI_CREATE_FUNCTION,
    NAPI_CREATE_HANDLER_ARG, NAPI_CREATE_NAME_ARG,
};
use crate::node::{named_children, span};

impl<'a> Cx<'a> {
    pub(super) fn napi_call(&self, cv: &CallView<'_>) {
        match self.lang {
            Language::C | Language::Cpp => {
                // Node-API `napi_create_function(env, "name", len, fn, ..)` (the runtime ABI),
                // else a `registration_call` row (a registration macro: name and handler
                // arguments from the row).
                let registration = if cv.path.len() != 1 {
                    None
                } else if cv.member == NAPI_CREATE_FUNCTION {
                    Some((BridgeKind::Napi, NAPI_CREATE_NAME_ARG, NAPI_CREATE_HANDLER_ARG))
                } else {
                    self.rows("registration_call")
                        .filter(|r| r.symbol.as_deref() == Some(cv.member.as_str()))
                        .find_map(|r| Some((r.bridge?, r.key_pos?, r.handler_pos?)))
                };
                if let Some((bridge, name_arg, handler_arg)) = registration {
                    let name = cv.positional(name_arg).and_then(|n| self.eval(n, 0).plain());
                    let handler = cv.positional(handler_arg);
                    if let (Some(name), Some(h)) = (name, handler) {
                        let mut detail = vec![("api".to_string(), cv.member.clone())];
                        let decl = self.handler_info(h, &mut detail);
                        self.push(
                            bridge,
                            BoundaryRole::Provides,
                            name,
                            cv.owner,
                            decl,
                            span(cv.node),
                            detail,
                        );
                    }
                    return;
                }
                self.addon_export_member(cv);
            }
            Language::JavaScript | Language::TypeScript | Language::Tsx => {
                if let Some((_, Bind::Addon { module })) = self.bind_of_receiver(cv) {
                    let mut detail = vec![("loader".to_string(), "require".to_string())];
                    if !module.is_empty() {
                        detail.push(("module".into(), module.clone()));
                    }
                    self.push(
                        BridgeKind::Napi,
                        BoundaryRole::Uses,
                        cv.member.clone(),
                        cv.owner,
                        None,
                        span(cv.node),
                        detail,
                    );
                }
            }
            _ => {}
        }
    }

    /// `exports.Set("name", Function::New(env, fn))`: an `addon_export_member` row names the
    /// member, an `addon_function_constructor` row the wrapping constructor (matched by its
    /// last two segments, as C++ code spells it after `using namespace`).
    pub(super) fn addon_export_member(&self, cv: &CallView<'_>) {
        if cv.positional_count() != 2 {
            return;
        }
        let Some(row) = self
            .rows("addon_export_member")
            .find(|r| r.symbol.as_deref().is_some_and(|s| last_segment(s) == cv.member))
        else {
            return;
        };
        let Some(bridge) = row.bridge else { return };
        let Some(name_node) = cv.positional(0) else { return };
        let name = self.eval(name_node, 0).plain().or_else(|| {
            let inner = self.call_view(self.unwrap(name_node))?;
            inner.positional(1).and_then(|n| self.eval(n, 0).plain())
        });
        let Some(value) = cv.positional(1).map(|n| self.unwrap(n)) else { return };
        let Some(inner) = self.call_view(value) else { return };
        let written = inner.path.join(".");
        let wraps = self
            .rows_of("addon_function_constructor", bridge)
            .filter_map(|r| r.symbol.as_deref())
            .any(|s| {
                let segs = symbol_segments(s);
                let tail = segs[segs.len().saturating_sub(2)..].join(".");
                !tail.is_empty() && written.ends_with(&tail)
            });
        if !wraps {
            return;
        }
        let (Some(name), Some(f)) = (name, inner.positional(1)) else { return };
        let mut detail = vec![("api".to_string(), row.symbol.clone().unwrap_or_default())];
        let decl = self.handler_info(f, &mut detail);
        self.push(bridge, BoundaryRole::Provides, name, cv.owner, decl, span(cv.node), detail);
    }
}

impl<'a> Cx<'a> {
    pub(super) fn napi_descriptors(&self, node: Node<'_>) {
        let Some(init) = crate::node::find_descendant(node, 64, |n| n.kind() == "initializer_list") else {
            return;
        };
        for row in named_children(init) {
            if row.kind() != "initializer_list" {
                continue;
            }
            let items = named_children(row);
            let Some(name) = items.first().and_then(|n| self.eval(*n, 0).plain()) else { continue };
            let Some(func) = items.get(2) else { continue };
            if func.kind() != "identifier" {
                continue;
            }
            let mut detail = vec![("api".to_string(), "napi_property_descriptor".to_string())];
            let decl = self.handler_info(*func, &mut detail);
            self.push(BridgeKind::Napi, BoundaryRole::Provides, name, None, decl, span(row), detail);
        }
    }
}
