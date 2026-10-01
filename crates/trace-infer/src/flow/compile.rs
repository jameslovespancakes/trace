//! Compilation of the index facts into flow constraints, and test provenance of files and
//! symbols (child of [`crate::flow`]).

use super::*;

/// Test files by path conventions (index = file id).
pub(crate) fn test_files(index: &Index) -> Vec<bool> {
    index
        .files
        .iter()
        .map(|f| {
            trace_syntax::is_test_path(&f.path, f.language, &trace_syntax::testing::TestConfig::default())
        })
        .collect()
}

/// Test-code symbols (index = symbol id): test declarations, symbols of test files and
/// every scope nested in them.
pub(crate) fn test_symbols(index: &Index) -> Vec<bool> {
    let files = test_files(index);
    let mut tests = vec![false; index.symbols.len()];
    for s in &index.symbols {
        let inherited = s.parent.is_some_and(|p| p.idx() < tests.len() && tests[p.idx()]);
        tests[s.id.idx()] = s.is_test || files[s.file.idx()] || inherited;
    }
    tests
}

pub(super) fn value_of(index: &Index, target: SymbolId) -> Value {
    if index.symbol(target).kind.is_type() {
        Value::Class(target)
    } else {
        Value::Function(target)
    }
}

/// First byte of an expression node, when known.
pub(super) fn node_start(node: &Node) -> Option<u32> {
    match node {
        Node::Name { span, .. } => Some(span.start),
        Node::Attr { object, span, .. } => node_start(object).or(Some(span.start)),
        Node::Call(c) => Some(c.span.start),
        Node::Object { span, .. } => Some(span.start),
        Node::Lambda(_)
        | Node::Choice(_)
        | Node::Yielded { .. }
        | Node::Literal
        | Node::Module(_)
        | Node::Opaque => None,
    }
}

/// End byte of an expression node, when known.
pub(super) fn node_end(node: &Node) -> Option<u32> {
    match node {
        Node::Name { span, .. } | Node::Attr { span, .. } | Node::Object { span, .. } => Some(span.end),
        Node::Call(c) => Some(c.span.end),
        Node::Lambda(_)
        | Node::Choice(_)
        | Node::Yielded { .. }
        | Node::Literal
        | Node::Module(_)
        | Node::Opaque => None,
    }
}

pub(super) fn function_ids<'v>(vals: impl IntoIterator<Item = &'v Value>) -> BTreeSet<SymbolId> {
    vals.into_iter()
        .filter_map(|v| match *v {
            Value::Function(f) | Value::Bound(f, _) => Some(f),
            _ => None,
        })
        .collect()
}

/// Edge kinds whose evidence point marks a proven callee.
pub(super) const CALL_KINDS: [EdgeKind; 6] = [
    EdgeKind::Calls,
    EdgeKind::Awaits,
    EdgeKind::Constructor,
    EdgeKind::CreatesCoroutine,
    EdgeKind::CreatesGenerator,
    EdgeKind::Iterates,
];

/// Compiles per-file facts into constraints (global ids, interned names, resolved refs).
pub(super) struct Compiler<'c> {
    pub(super) index: &'c Index,
    pub(super) names: &'c mut Interner,
    pub(super) refs: &'c HashMap<(FileId, ByteSpan), SymbolId>,
    pub(super) lambdas: &'c HashMap<(FileId, ByteSpan), SymbolId>,
    pub(super) anonymous: &'c mut HashSet<SymbolId>,
    /// Identity guards of this file's calls by call span.
    pub(super) guards: HashMap<ByteSpan, &'c [Expr]>,
    /// Library knowledge of this file's calls by callee start byte.
    pub(super) knowledge: HashMap<u32, &'c CallBehaviour>,
    pub(super) file: FileId,
    /// The `super(..)` call of this file's language ([`super_call`]).
    pub(super) super_call: Option<&'static str>,
    /// Library symbol of every call of this file the server resolved into library code, by
    /// callee span.
    pub(super) library_at: HashMap<ByteSpan, String>,
    /// Spans of this file's import bindings (module loader calls inside them make no object).
    pub(super) loaders: Vec<ByteSpan>,
    /// Library symbols interned for `Value::Library` (shared by every file).
    pub(super) libraries: &'c mut Vec<String>,
    pub(super) library_ids: &'c mut HashMap<String, u32>,
    /// CommonJS module rules when this file is a JavaScript file.
    pub(super) js_objects: Option<&'c JsObjects>,
}

/// Library knowledge of one file's calls by callee start byte.
pub(super) fn file_knowledge<'k>(
    knowledge: &'k LibraryKnowledge,
    path: &str,
) -> HashMap<u32, &'k CallBehaviour> {
    let lo = (path.to_string(), 0u32);
    let hi = (path.to_string(), u32::MAX);
    knowledge
        .by_call
        .range(lo..=hi)
        .map(|((_, start), b)| (*start, b))
        .collect()
}

impl Compiler<'_> {
    pub(super) fn decl(&self, local: u32) -> Option<SymbolId> {
        self.index.file(self.file).symbol_of_decl(local)
    }

    /// Library object id of a call (`CallNode::library`).
    pub(super) fn library(&mut self, func_span: ByteSpan, span: ByteSpan) -> Option<u32> {
        if self
            .loaders
            .iter()
            .any(|l| l.start <= span.start && span.end <= l.end)
        {
            return None;
        }
        let text = self
            .library_at
            .get(&func_span)
            .cloned()
            .or_else(|| self.knowledge.get(&func_span.start).and_then(|b| b.symbol.clone()))
            .filter(|t| !t.is_empty())?;
        if let Some(&id) = self.library_ids.get(&text) {
            return Some(id);
        }
        let id = self.libraries.len() as u32;
        self.libraries.push(text.clone());
        self.library_ids.insert(text, id);
        Some(id)
    }

    pub(super) fn scope(&self, scope: Scope) -> Option<ScopeKey> {
        match scope {
            Scope::Module => Some(ScopeKey::Module(self.file)),
            Scope::Decl(d) => self.decl(d).map(ScopeKey::Symbol),
        }
    }

    pub(super) fn node(&mut self, expr: &Expr) -> Node {
        match expr {
            Expr::Name { name, .. } if name.starts_with(LITERAL_PREFIX) => Node::Literal,
            Expr::Name { name, span } => Node::Name {
                name: self.names.intern(name),
                target: self.refs.get(&(self.file, *span)).copied(),
                span: *span,
            },
            Expr::Call {
                func, kwargs, span, ..
            } if matches!(func.as_ref(), Expr::Name { name, .. } if name == OBJECT_CALLEE) => Node::Object {
                alloc: span.start,
                span: *span,
                fields: kwargs
                    .iter()
                    .map(|(k, v)| (self.names.intern(k), self.node(v)))
                    .collect(),
            },
            Expr::Attr { object, attr, .. }
                if self.js_objects.is_some_and(|m| m.is_export_read(object, attr)) =>
            {
                Node::Module(self.file)
            }
            Expr::Call { func, span, .. } if self.loaded_module(func, *span).is_some() => {
                self.loaded_module(func, *span).map_or(Node::Opaque, Node::Module)
            }
            Expr::Attr {
                object,
                attr,
                attr_span,
                ..
            } => Node::Attr {
                object: Box::new(self.node(object)),
                attr: self.names.intern(attr),
                target: self.refs.get(&(self.file, *attr_span)).copied(),
                span: *attr_span,
            },
            Expr::Call { .. } => match self.call(expr) {
                Some(c) => Node::Call(Box::new(c)),
                None => Node::Opaque,
            },
            Expr::Choice(options) => Node::Choice(options.iter().map(|e| self.node(e)).collect()),
            Expr::Await(inner) => self.node(inner),
            Expr::Lambda { span, function } => {
                // The declaration the lowering names, else the callable spanning the value.
                let declared = (*function)
                    .and_then(|d| self.decl(d))
                    .filter(|s| self.index.symbol(*s).kind.is_callable());
                match declared.or_else(|| self.lambdas.get(&(self.file, *span)).copied()) {
                    Some(symbol) => {
                        self.anonymous.insert(symbol);
                        Node::Lambda(symbol)
                    }
                    None => Node::Opaque,
                }
            }
            Expr::Opaque => Node::Opaque,
        }
    }

    /// Library effects of a call on its arguments and on the function its result decorates.
    /// A call whose callee is itself a call applies the `Decorates` effects of that inner
    /// call (both callees start at the same byte). Python's zero-argument `super()` returns
    /// the current receiver (language rule, PEP 3135; `super(C, self)` names the same
    /// receiver explicitly).
    pub(super) fn effects(
        &self,
        func: &Expr,
        func_span: ByteSpan,
        positional: usize,
    ) -> (Vec<Effect>, Vec<Effect>) {
        let application = matches!(func, Expr::Call { .. });
        let (mut effects, decorates) = match self.knowledge.get(&func_span.start) {
            Some(b) => (
                applied_effects(b, application),
                if application {
                    Vec::new()
                } else {
                    applied_effects(b, true)
                },
            ),
            None => (Vec::new(), Vec::new()),
        };
        if let Some(m) = self.js_objects.filter(|_| !effects.iter().any(links_members)) {
            effects.extend(m.builtin_effects(func, |root| self.refs.contains_key(&(self.file, root))));
        }
        if self.super_call.is_some()
            && (positional == 0 || positional == 2)
            && matches!(func, Expr::Name { name, .. } if Some(name.as_str()) == self.super_call)
            && !effects.contains(&Effect::Receiver)
        {
            effects.push(Effect::Receiver);
        }
        (effects, decorates)
    }

    /// Library effects of applying decorator `d` to the decorated function (argument 0): a
    /// decorator factory call's `Decorates` effects, else the knowledge of the decorator
    /// expression itself (the application's callee starts where the expression starts).
    fn decorator_effects(&self, d: &Expr) -> Vec<Effect> {
        match d {
            Expr::Call {
                func,
                func_span,
                args,
                ..
            } => self.effects(func, *func_span, args.len()).1,
            other => other
                .span()
                .and_then(|s| self.knowledge.get(&s.start))
                .map(|b| applied_effects(b, false))
                .unwrap_or_default(),
        }
    }

    pub(super) fn call(&mut self, expr: &Expr) -> Option<CallNode> {
        let Expr::Call {
            func,
            func_span,
            args,
            kwargs,
            span,
            ..
        } = expr
        else {
            return None;
        };
        let (effects, _) = self.effects(func, *func_span, args.len());
        let guards: &[Expr] = self.guards.get(span).copied().unwrap_or(&[]);
        let not_identical = guards.iter().map(|g| self.node(g)).collect();
        let library = self.library(*func_span, *span);
        Some(CallNode {
            func: self.node(func),
            func_span: *func_span,
            span: *span,
            args: args.iter().map(|a| self.node(a)).collect(),
            kwargs: kwargs
                .iter()
                .map(|(k, v)| (self.names.intern(k), self.node(v)))
                .collect(),
            effects,
            not_identical,
            library,
        })
    }

    pub(super) fn target(&mut self, target: &BindTarget) -> Option<Target> {
        Some(match target {
            BindTarget::Var { scope, name } => Target::Var(self.scope(*scope)?, self.names.intern(name)),
            BindTarget::Member { class, name } => {
                let class = self.decl(*class)?;
                if !self.index.symbol(class).kind.is_type() {
                    return None;
                }
                Target::Member(class, self.names.intern(name))
            }
            BindTarget::Field { name } => Target::Field(self.names.intern(name)),
            BindTarget::FieldOf { .. } if self.js_objects.is_some_and(|m| m.is_export_target(target)) => {
                let exports = self.js_objects?.exports_name();
                Target::Var(ScopeKey::Module(self.file), self.names.intern(exports))
            }
            BindTarget::FieldOf { object, name } => {
                Target::FieldOf(self.node(object), self.names.intern(name))
            }
        })
    }

    /// The repository file a loader call of this file loads ([`crate::flow::js_objects`]): never
    /// when the server resolved the loader name to a repository declaration.
    fn loaded_module(&self, func: &Expr, span: ByteSpan) -> Option<FileId> {
        let m = self.js_objects?;
        if let Expr::Name { span, .. } = func {
            if self.refs.contains_key(&(self.file, *span)) {
                return None;
            }
        }
        m.loaded_file(self.index, self.file, func, span)
    }

    pub(super) fn self_param(
        &mut self,
        function: u32,
        param: &str,
        class: u32,
        is_class: bool,
    ) -> Option<(SymbolId, SelfParam)> {
        let function = self.decl(function)?;
        let class = self.decl(class)?;
        if !self.index.symbol(class).kind.is_type() {
            return None;
        }
        Some((
            function,
            SelfParam {
                name: self.names.intern(param),
                class,
                is_class,
            },
        ))
    }

    pub(super) fn constraint(&mut self, fact: &FlowFact) -> Option<Constraint> {
        let file = self.file;
        let (rule, scope) = match fact {
            FlowFact::Bind { target, value, scope } => {
                let scope = self.scope(*scope)?;
                let target = self.target(target)?;
                (
                    Rule::Bind {
                        target,
                        value: self.node(value),
                    },
                    scope,
                )
            }
            FlowFact::Return { function, value } => {
                let function = self.decl(*function)?;
                (
                    Rule::Return {
                        function,
                        value: self.node(value),
                    },
                    ScopeKey::Symbol(function),
                )
            }
            FlowFact::Eval { scope, call } => {
                let scope = self.scope(*scope)?;
                (
                    Rule::Eval {
                        call: self.call(call)?,
                    },
                    scope,
                )
            }
            FlowFact::Decorated {
                scope,
                target,
                function,
                decorators,
            } => {
                let scope = self.scope(*scope)?;
                let function = self.decl(*function)?;
                let target = self.target(target)?;
                let fallback = self.index.symbol(function).span.bytes.start;
                let decorators = decorators
                    .iter()
                    .map(|d| {
                        let alloc = d.span().map_or(fallback, |s| s.start);
                        let effects = self.decorator_effects(d);
                        (self.node(d), alloc, effects)
                    })
                    .collect();
                (
                    Rule::Decorated {
                        target,
                        function,
                        decorators,
                    },
                    scope,
                )
            }
            FlowFact::ImplicitSelf { .. } => return None,
        };
        Some(Constraint {
            rule,
            scope,
            file,
            test: false,
        })
    }
}
