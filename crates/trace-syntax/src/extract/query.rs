//! Running the language query: capture roles and the pending declarations, calls,
//! references and relations it finds (capture contract SPEC 6.2).

use std::collections::{HashMap, HashSet};

use streaming_iterator::StreamingIterator;
use trace_core::facts::ParamKind;
use trace_core::model::SymbolKind;
use tree_sitter::{Node, QueryCursor};

use crate::grammar::Grammar;

#[derive(Clone, Copy, Debug)]
enum Role {
    Def(SymbolKind),
    Name,
    Body,
    Decorator,
    Base,
    Param(ParamKind),
    ParamDefault,
    Doc,
    Container,
    Call,
    New,
    Callee,
    ImplType,
    ImplTrait,
    TestName,
    TestBlock,
    /// `@stub`: the definition is a declaration that is not a definition (Haskell type
    /// signatures), `is_stub = true` whatever the language's bodiless rule says.
    Stub,
    Other,
}

fn role(name: &str) -> Role {
    match name {
        "definition.function" => Role::Def(SymbolKind::Function),
        "definition.method" => Role::Def(SymbolKind::Method),
        "definition.constructor" => Role::Def(SymbolKind::Constructor),
        "definition.class" => Role::Def(SymbolKind::Class),
        "definition.interface" => Role::Def(SymbolKind::Interface),
        "name" => Role::Name,
        "body" => Role::Body,
        "decorator" => Role::Decorator,
        "base" => Role::Base,
        "parameter" => Role::Param(ParamKind::Positional),
        "parameter.keyword" => Role::Param(ParamKind::KeywordOnly),
        "parameter.variadic" => Role::Param(ParamKind::VarPositional),
        "parameter.default" => Role::ParamDefault,
        "doc" => Role::Doc,
        "container" => Role::Container,
        "reference.call" => Role::Call,
        "reference.new" => Role::New,
        "callee" => Role::Callee,
        "impl.type" => Role::ImplType,
        "impl.trait" => Role::ImplTrait,
        "test.name" => Role::TestName,
        "test.block" => Role::TestBlock,
        "stub" => Role::Stub,
        _ => Role::Other,
    }
}

/// Specificity when several patterns capture the same definition node.
fn kind_rank(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Function | SymbolKind::Class => 1,
        SymbolKind::Method | SymbolKind::Interface => 2,
        SymbolKind::Constructor => 3,
        // Never produced by queries (synthetic `<module>` declaration).
        SymbolKind::Module => 0,
    }
}

/// A definition found by the query (merged over all matches of the same node).
pub(crate) struct Pending<'t> {
    pub def: Node<'t>,
    pub kind: SymbolKind,
    pub name: Option<Node<'t>>,
    pub body: Option<Node<'t>>,
    pub decorators: Vec<Node<'t>>,
    pub bases: Vec<Node<'t>>,
    pub params: Vec<(Node<'t>, ParamKind, Option<Node<'t>>)>,
    pub doc: Option<Node<'t>>,
    pub container: Option<Node<'t>>,
    /// Captured `@stub` (see [`Role::Stub`]).
    pub stub: bool,
}

/// A call found by the query.
pub(crate) struct CallCapture<'t> {
    pub call: Node<'t>,
    pub callee: Option<Node<'t>>,
    pub is_new: bool,
}

/// Everything the query found in one tree.
pub(crate) struct Captures<'t> {
    pub defs: HashMap<usize, Pending<'t>>,
    pub calls: HashMap<usize, CallCapture<'t>>,
    /// `(type node, trait node)` of out-of-line implementations.
    pub impls: Vec<(Node<'t>, Node<'t>)>,
    /// `(block node, name node)` of test blocks.
    pub tests: Vec<(Node<'t>, Node<'t>)>,
}

pub(super) fn push_unique<'t>(list: &mut Vec<Node<'t>>, node: Node<'t>) {
    if !list.iter().any(|n| n.id() == node.id()) {
        list.push(node);
    }
}

/// Run the grammar query over the tree and merge matches per definition / call node.
pub(crate) fn run_query<'t>(grammar: &Grammar, root: Node<'t>, source: &[u8]) -> Captures<'t> {
    let roles: Vec<Role> = grammar.query.capture_names().iter().map(|n| role(n)).collect();
    let mut caps = Captures {
        defs: HashMap::new(),
        calls: HashMap::new(),
        impls: Vec::new(),
        tests: Vec::new(),
    };
    let mut seen_impls: HashSet<(usize, usize)> = HashSet::new();
    let mut seen_tests: HashSet<usize> = HashSet::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&grammar.query, root, source);
    while let Some(m) = matches.next() {
        let mut def: Option<(Node<'t>, SymbolKind)> = None;
        let mut name = None;
        let mut body = None;
        let mut doc = None;
        let mut container = None;
        let mut decorators: Vec<Node<'t>> = Vec::new();
        let mut bases: Vec<Node<'t>> = Vec::new();
        let mut params: Vec<(Node<'t>, ParamKind, Option<Node<'t>>)> = Vec::new();
        let mut call = None;
        let mut is_new = false;
        let mut callee = None;
        let mut impl_type = None;
        let mut impl_trait = None;
        let mut test_name = None;
        let mut test_block = None;
        let mut stub = false;
        for capture in m.captures {
            let node = capture.node;
            match roles.get(capture.index as usize).copied().unwrap_or(Role::Other) {
                Role::Def(kind) => def = Some((node, kind)),
                Role::Name => name = name.or(Some(node)),
                Role::Body => body = body.or(Some(node)),
                Role::Decorator => push_unique(&mut decorators, node),
                Role::Base => push_unique(&mut bases, node),
                Role::Param(kind) => {
                    if !params.iter().any(|(n, _, _)| n.id() == node.id()) {
                        params.push((node, kind, None));
                    }
                }
                Role::ParamDefault => {
                    if let Some(last) = params.last_mut() {
                        last.2 = Some(node);
                    }
                }
                Role::Doc => doc = doc.or(Some(node)),
                Role::Container => container = container.or(Some(node)),
                Role::Call => call = Some(node),
                Role::New => {
                    call = Some(node);
                    is_new = true;
                }
                Role::Callee => callee = callee.or(Some(node)),
                Role::ImplType => impl_type = Some(node),
                Role::ImplTrait => impl_trait = Some(node),
                Role::TestName => test_name = Some(node),
                Role::TestBlock => test_block = Some(node),
                Role::Stub => stub = true,
                Role::Other => {}
            }
        }
        if let Some((node, kind)) = def {
            let entry = caps.defs.entry(node.id()).or_insert_with(|| Pending {
                def: node,
                kind,
                name: None,
                body: None,
                decorators: Vec::new(),
                bases: Vec::new(),
                params: Vec::new(),
                doc: None,
                container: None,
                stub: false,
            });
            entry.stub |= stub;
            if kind_rank(kind) > kind_rank(entry.kind) {
                entry.kind = kind;
            }
            entry.name = entry.name.or(name);
            entry.body = entry.body.or(body);
            entry.doc = entry.doc.or(doc);
            entry.container = entry.container.or(container);
            for d in decorators {
                push_unique(&mut entry.decorators, d);
            }
            for b in bases {
                push_unique(&mut entry.bases, b);
            }
            for p in params {
                if !entry.params.iter().any(|(n, _, _)| n.id() == p.0.id()) {
                    entry.params.push(p);
                }
            }
        }
        if let Some(node) = call {
            let entry = caps.calls.entry(node.id()).or_insert(CallCapture {
                call: node,
                callee: None,
                is_new,
            });
            entry.callee = entry.callee.or(callee);
            entry.is_new |= is_new;
        }
        if let (Some(t), Some(tr)) = (impl_type, impl_trait) {
            if seen_impls.insert((t.id(), tr.id())) {
                caps.impls.push((t, tr));
            }
        }
        if let (Some(block), Some(name)) = (test_block, test_name) {
            if seen_tests.insert(block.id()) {
                caps.tests.push((block, name));
            }
        }
    }
    // Definitions sharing one body (a named function expression bound by `const f = ...`)
    // are one declaration: keep the outermost node.
    let mut by_body: HashMap<usize, usize> = HashMap::new();
    let mut dropped: Vec<usize> = Vec::new();
    let mut ids: Vec<usize> = caps.defs.keys().copied().collect();
    ids.sort_unstable_by_key(|id| {
        let d = &caps.defs[id];
        (d.def.start_byte(), std::cmp::Reverse(d.def.end_byte()))
    });
    for id in ids {
        let Some(body) = caps.defs[&id].body else {
            continue;
        };
        if by_body.insert(body.id(), id).is_some() {
            dropped.push(id);
        }
    }
    for id in dropped {
        caps.defs.remove(&id);
    }
    for pending in caps.defs.values_mut() {
        pending.decorators.sort_by_key(|n| n.start_byte());
        pending.bases.sort_by_key(|n| n.start_byte());
        pending.params.sort_by_key(|(n, _, _)| n.start_byte());
    }
    caps
}
