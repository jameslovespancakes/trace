//! Test fixtures: hand-built syntax facts + semantics assembled into a real [`Index`].
//!
//! Spans for flow-only fixtures are synthetic (flow never reads source); packet tests write
//! real files under `<temp>/trace-tests/trace-infer-*` and use exact spans found in the
//! fixture text (test data construction, not source extraction).

use trace_core::assemble::{assemble, AssembleInput};
use trace_core::facts::{
    Activation, BindTarget, CallDetail, CallSite, CallbackArg, Declaration, Export, Expr, FileFacts,
    FlowFact, ImplRelation, ImplicitKind, ImplicitOp, Import, ImportKind, Param, ParamKind, Scope, TypeFact,
    TypeSource, TypeSubject,
};
use trace_core::semantics::{FileSemantics, SemEdge, SemImplementation, SemUnresolved, SemValueRef};
use trace_core::{
    ByteSpan, EdgeKind, ExecutionModel, FileRecord, Hash32, Index, IndexHeader, Language, Provider,
    Resolution, Span, SupportLevel, SymbolId, SymbolKind, UnresolvedKind,
};

/// Handle to a declaration of a fixture file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct D {
    pub file: usize,
    pub decl: u32,
}

/// Declaration builder.
#[derive(Clone, Debug)]
pub struct Decl {
    name: String,
    kind: SymbolKind,
    parent: Option<D>,
    bases: Vec<String>,
    params: Vec<String>,
    decorators: Vec<String>,
    stub: bool,
    test: bool,
    execution: ExecutionModel,
    container: Option<String>,
    span: Option<ByteSpan>,
    /// Full parameter specs (kind, default); overrides `params` when set.
    specs: Option<Vec<Param>>,
    /// Explicit qualified name (namespaces, out-of-line containers); default: parent's
    /// qualified name + name.
    qualified: Option<String>,
}

impl Decl {
    fn new(name: &str, kind: SymbolKind, parent: Option<D>) -> Decl {
        Decl {
            name: name.into(),
            kind,
            parent,
            bases: Vec::new(),
            params: Vec::new(),
            decorators: Vec::new(),
            stub: false,
            test: false,
            execution: ExecutionModel::Ordinary,
            container: None,
            span: None,
            specs: None,
            qualified: None,
        }
    }
    /// Explicit qualified name (`ns.W.draw`, `W.draw` of an out-of-line definition).
    pub fn qualified(mut self, qualified: &str) -> Decl {
        self.qualified = Some(qualified.into());
        self
    }
    /// Constructor declaration (`SymbolKind::Constructor`) of a class.
    pub fn constructor(name: &str, class: D) -> Decl {
        Decl::new(name, SymbolKind::Constructor, Some(class))
    }
    /// Parameters with kinds and defaults: `(name, kind, has_default)`.
    pub fn param_specs(mut self, specs: &[(&str, ParamKind, bool)]) -> Decl {
        self.params = specs.iter().map(|(n, _, _)| n.to_string()).collect();
        self.specs = Some(
            specs
                .iter()
                .map(|(n, k, d)| Param {
                    name: n.to_string(),
                    kind: *k,
                    has_default: *d,
                })
                .collect(),
        );
        self
    }
    pub fn function(name: &str) -> Decl {
        Decl::new(name, SymbolKind::Function, None)
    }
    pub fn class(name: &str) -> Decl {
        Decl::new(name, SymbolKind::Class, None)
    }
    pub fn interface(name: &str) -> Decl {
        Decl::new(name, SymbolKind::Interface, None)
    }
    pub fn method(name: &str, class: D) -> Decl {
        Decl::new(name, SymbolKind::Method, Some(class))
    }
    /// Function nested in another declaration (closures, lambdas, generator expressions).
    pub fn nested(name: &str, parent: D) -> Decl {
        Decl::new(name, SymbolKind::Function, Some(parent))
    }
    /// Anonymous lambda scope declared inside `parent` (syntax contract, SPEC §6.3).
    pub fn lambda(parent: D) -> Decl {
        Decl::nested("<lambda>", parent)
    }
    /// Generator-expression scope declared inside `parent` (syntax contract, SPEC §6.3).
    pub fn genexpr(parent: D) -> Decl {
        Decl::nested("<genexpr>", parent).execution(ExecutionModel::Generator)
    }
    pub fn execution(mut self, execution: ExecutionModel) -> Decl {
        self.execution = execution;
        self
    }
    pub fn test(mut self) -> Decl {
        self.test = true;
        self
    }
    pub fn bases(mut self, bases: &[&str]) -> Decl {
        self.bases = bases.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn params(mut self, params: &[&str]) -> Decl {
        self.params = params.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn decorators(mut self, decorators: &[&str]) -> Decl {
        self.decorators = decorators.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn stub(mut self) -> Decl {
        self.stub = true;
        self
    }
    pub fn container(mut self, container: &str) -> Decl {
        self.container = Some(container.into());
        self
    }
    pub fn span(mut self, span: ByteSpan) -> Decl {
        self.span = Some(span);
        self
    }
}

struct FixFile {
    path: String,
    language: Language,
    source: Option<String>,
    facts: FileFacts,
    semantic: FileSemantics,
    /// The server answered no call of the file with a target: every syntax call without a
    /// semantic edge or unresolved entry becomes a `no_semantic_target` unknown at build.
    blind: bool,
}

pub struct Fixture {
    files: Vec<FixFile>,
    root: String,
    next_byte: u32,
}

impl Default for Fixture {
    fn default() -> Self {
        Fixture::new()
    }
}

/// Semantics of a blind file: the recorded semantics plus a `no_semantic_target` entry for
/// every syntax call the server answered nothing for.
fn blind_semantics(f: &FixFile) -> FileSemantics {
    let mut s = f.semantic.clone();
    let answered: std::collections::HashSet<ByteSpan> = s
        .edges
        .iter()
        .map(|e| e.at)
        .chain(s.unresolved.iter().map(|u| u.at))
        .collect();
    for c in &f.facts.calls {
        if answered.contains(&c.callee_span) {
            continue;
        }
        s.unresolved.push(SemUnresolved {
            owner: c.owner,
            kind: UnresolvedKind::NoSemanticTarget,
            at: c.callee_span,
            line: c.line,
            callee: c.callee.clone(),
            candidates: Vec::new(),
        });
    }
    s
}

/// Canonical display form of a directory (what `IndexHeader::root` holds).
pub fn canonical(dir: &std::path::Path) -> String {
    trace_core::inventory::strip_verbatim(std::fs::canonicalize(dir).expect("canonicalize"))
        .to_string_lossy()
        .into_owned()
}

impl Fixture {
    pub fn new() -> Fixture {
        Fixture {
            files: Vec::new(),
            root: "fixture-root-never-read".into(),
            next_byte: 1_000_000,
        }
    }

    pub fn file(&mut self, path: &str, language: Language, source: Option<&str>) -> usize {
        self.files.push(FixFile {
            path: path.into(),
            language,
            source: source.map(str::to_string),
            facts: FileFacts {
                language: Some(language),
                ..FileFacts::default()
            },
            semantic: FileSemantics {
                provider: Provider::Pyright,
                tool_fingerprint: "fixture".into(),
                edges: Vec::new(),
                unresolved: Vec::new(),
                value_refs: Vec::new(),
                diagnostics: Vec::new(),
                implementations: Vec::new(),
                resolved_elsewhere: Vec::new(),
                callback_params: Vec::new(),
                library_files: Vec::new(),
                library_calls: Vec::new(),
                outside_build: None,
                expanded: Vec::new(),
                library_dispatch: Vec::new(),
                library_bases: Vec::new(),
            },
            blind: false,
        });
        self.files.len() - 1
    }

    /// A file whose language server named no target for any call (every call is blind:
    /// `no_semantic_target`, the input of name narrowing).
    pub fn blind_file(&mut self, path: &str, language: Language, source: Option<&str>) -> usize {
        let f = self.file(path, language, source);
        self.files[f].blind = true;
        f
    }

    /// The synthetic `<module>` declaration of a file (append after every other declaration).
    pub fn module(&mut self, file: usize) -> D {
        let span = self.span();
        let f = &mut self.files[file];
        f.facts.declarations.push(Declaration {
            name: "<module>".into(),
            qualified_name: "<module>".into(),
            kind: SymbolKind::Module,
            span: Span {
                bytes: ByteSpan::new(0, span.end),
                start_line: 1,
                end_line: 1,
            },
            name_span: ByteSpan::new(0, 0),
            body_start: 0,
            parent: None,
            container: None,
            doc: None,
            decorators: Vec::new(),
            bases: Vec::new(),
            parameters: Vec::new(),
            execution: ExecutionModel::Ordinary,
            is_stub: false,
            is_test: false,
            declaration_lines: Vec::new(),
            identifiers: Vec::new(),
        });
        let decl = (f.facts.declarations.len() - 1) as u32;
        f.facts.module_decl = Some(decl);
        D { file, decl }
    }

    /// `import` binding `local` to `target` at module level.
    pub fn import(&mut self, file: usize, local: &str, target: &str, kind: ImportKind) {
        let span = self.span();
        self.files[file].facts.imports.push(Import {
            local: local.into(),
            target: target.into(),
            kind,
            scope: Scope::Module,
            span,
            line: 1,
        });
    }

    /// Re-export of `target` as `exported`.
    pub fn export(&mut self, file: usize, exported: &str, target: &str) {
        let span = self.span();
        self.files[file].facts.exports.push(Export {
            exported: exported.into(),
            target: target.into(),
            span,
            line: 1,
        });
    }

    /// A call of `callee` with `args` positional arguments owned by `owner` (`None`:
    /// module level); returns the callee span.
    pub fn call_n(&mut self, file: usize, owner: Option<D>, callee: &str, args: u32) -> ByteSpan {
        let callee_span = self.span();
        let span = ByteSpan::new(callee_span.start, callee_span.end + 1);
        self.call_site(file, owner, span, callee_span, callee);
        let member = [".", "->", "::", ":"]
            .iter()
            .filter_map(|s| callee.rfind(s).map(|i| i + s.len()))
            .max()
            .map_or(callee, |i| &callee[i..]);
        let call = self.files[file].facts.calls.last_mut().expect("call");
        call.arg_count = args;
        call.member = Some(member.to_string());
        callee_span
    }

    /// Lexical owner of the last call (class-body calls: owner `None`, lexical the class).
    pub fn lexical_owner(&mut self, file: usize, lexical: D) {
        self.files[file].facts.calls.last_mut().expect("call").lexical_owner = Some(lexical.decl);
    }

    /// Semantic value reference at an explicit span.
    pub fn value_ref(&mut self, file: usize, span: ByteSpan, target: D) {
        let uid = self.uid(target);
        self.files[file].semantic.value_refs.push(SemValueRef {
            at: span,
            line: 1,
            target: uid,
        });
    }

    /// Out-of-line impl relation with an explicit span (methods inside it belong to it).
    pub fn impl_block(&mut self, file: usize, type_name: &str, trait_name: &str, span: ByteSpan) {
        self.files[file].facts.impls.push(ImplRelation {
            type_name: type_name.into(),
            trait_name: trait_name.into(),
            span,
        });
    }

    /// Unique synthetic span.
    pub fn span(&mut self) -> ByteSpan {
        let s = ByteSpan::new(self.next_byte, self.next_byte + 1);
        self.next_byte += 2;
        s
    }

    fn qualified(&self, d: D) -> String {
        self.files[d.file].facts.declarations[d.decl as usize]
            .qualified_name
            .clone()
    }

    pub fn uid(&self, d: D) -> String {
        format!("{}:{}", self.files[d.file].path, self.qualified(d))
    }

    pub fn decl(&mut self, file: usize, decl: Decl) -> D {
        let qualified = match (&decl.qualified, decl.parent) {
            (Some(q), _) => q.clone(),
            (None, Some(p)) => format!("{}.{}", self.qualified(p), decl.name),
            (None, None) => decl.name.clone(),
        };
        let span = match decl.span {
            Some(s) => s,
            None => self.span(),
        };
        let f = &mut self.files[file];
        f.facts.declarations.push(Declaration {
            name: decl.name,
            qualified_name: qualified,
            kind: decl.kind,
            span: Span {
                bytes: span,
                start_line: 1,
                end_line: 1,
            },
            name_span: span,
            body_start: span.end,
            parent: decl.parent.map(|p| p.decl),
            container: decl.container,
            doc: None,
            decorators: decl.decorators,
            bases: decl.bases,
            parameters: match decl.specs {
                Some(specs) => specs,
                None => decl
                    .params
                    .into_iter()
                    .map(|name| Param {
                        name,
                        kind: ParamKind::Positional,
                        has_default: false,
                    })
                    .collect(),
            },
            execution: decl.execution,
            is_stub: decl.stub,
            is_test: decl.test,
            declaration_lines: Vec::new(),
            identifiers: Vec::new(),
        });
        D {
            file,
            decl: (f.facts.declarations.len() - 1) as u32,
        }
    }

    /// Symbol id of a declaration in the built index (files sorted by path).
    pub fn id(&self, d: D) -> SymbolId {
        let path = &self.files[d.file].path;
        let before: usize = self
            .files
            .iter()
            .filter(|f| f.path < *path)
            .map(|f| f.facts.declarations.len())
            .sum();
        SymbolId(before as u32 + d.decl)
    }

    // --- expressions -------------------------------------------------------------------

    pub fn name(&mut self, name: &str) -> Expr {
        Expr::Name {
            name: name.into(),
            span: self.span(),
        }
    }

    /// Name that the semantic backend resolved to `target` (value reference).
    pub fn name_ref(&mut self, file: usize, name: &str, target: D) -> Expr {
        let span = self.span();
        let uid = self.uid(target);
        self.files[file].semantic.value_refs.push(SemValueRef {
            at: span,
            line: 1,
            target: uid,
        });
        Expr::Name {
            name: name.into(),
            span,
        }
    }

    pub fn attr(&mut self, object: Expr, attr: &str) -> Expr {
        let attr_span = self.span();
        Expr::Attr {
            object: Box::new(object),
            attr: attr.into(),
            attr_span,
            span: attr_span,
        }
    }

    pub fn call(&mut self, func: Expr, args: Vec<Expr>) -> Expr {
        self.call_kw(func, args, Vec::new())
    }

    pub fn call_kw(&mut self, func: Expr, args: Vec<Expr>, kwargs: Vec<(&str, Expr)>) -> Expr {
        let func_span = func.span().unwrap_or_else(|| self.span());
        // A call starts where its callee starts (library knowledge is keyed by the callee
        // start, which is the call's start in every grammar) and covers its arguments.
        let end = self.span().end;
        let span = ByteSpan::new(func_span.start.min(end), end);
        Expr::Call {
            func: Box::new(func),
            func_span,
            args,
            kwargs: kwargs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            span,
            is_new: false,
        }
    }

    /// Value of an anonymous scope declaration (`Expr::Lambda` spanning the declaration).
    pub fn lambda_expr(&self, d: D) -> Expr {
        Expr::Lambda {
            span: self.decl_span(d),
            function: Some(d.decl),
        }
    }

    /// Declaration span (argument spans of anonymous scopes).
    pub fn decl_span(&self, d: D) -> ByteSpan {
        self.files[d.file].facts.declarations[d.decl as usize].span.bytes
    }

    // --- facts -------------------------------------------------------------------------

    pub fn flow(&mut self, file: usize, fact: FlowFact) {
        self.files[file].facts.flow.push(fact);
    }

    /// The language's scoping proves the bare name at `span` local (`FileFacts::local_spans`).
    pub fn local(&mut self, file: usize, span: ByteSpan) {
        let spans = &mut self.files[file].facts.local_spans;
        spans.push(span);
        spans.sort_unstable_by_key(|s| (s.start, s.end));
        spans.dedup();
    }

    /// `ImplicitSelf{method, param, class, is_class}`.
    pub fn receiver(&mut self, file: usize, method: D, param: &str, class: D, is_class: bool) {
        self.flow(
            file,
            FlowFact::ImplicitSelf {
                function: method.decl,
                param: param.into(),
                class: class.decl,
                is_class,
            },
        );
    }

    /// `Bind{target, value, scope}`.
    pub fn bind(&mut self, file: usize, target: BindTarget, value: Expr, scope: Scope) {
        self.flow(file, FlowFact::Bind { target, value, scope });
    }

    /// `Return{function, value}`.
    pub fn ret(&mut self, file: usize, function: D, value: Expr) {
        self.flow(
            file,
            FlowFact::Return {
                function: function.decl,
                value,
            },
        );
    }

    /// Class-body member binding `name = value` (member and field-name slots).
    pub fn member(&mut self, file: usize, class: D, name: &str, value: Expr) {
        self.bind(
            file,
            BindTarget::Member {
                class: class.decl,
                name: name.into(),
            },
            value.clone(),
            Scope::Module,
        );
        self.bind(file, BindTarget::Field { name: name.into() }, value, Scope::Module);
    }

    /// `<receiver>.<name> = value` inside `method` (field-name and class-specific slots).
    pub fn field_store(&mut self, file: usize, method: D, receiver: &str, name: &str, value: Expr) {
        let scope = Scope::Decl(method.decl);
        self.bind(file, BindTarget::Field { name: name.into() }, value.clone(), scope);
        let object = self.name(receiver);
        self.bind(
            file,
            BindTarget::FieldOf {
                object,
                name: name.into(),
            },
            value,
            scope,
        );
    }

    /// `@decorators def function` bound as a class member (decorators outermost first).
    pub fn decorated_member(
        &mut self,
        file: usize,
        class: D,
        function: D,
        name: &str,
        decorators: Vec<Expr>,
    ) {
        self.flow(
            file,
            FlowFact::Decorated {
                scope: Scope::Module,
                target: BindTarget::Member {
                    class: class.decl,
                    name: name.into(),
                },
                function: function.decl,
                decorators,
            },
        );
    }

    /// `Eval{scope, call}` plus the matching syntax call site; returns the callee span.
    pub fn eval(&mut self, file: usize, owner: D, call: Expr, callee: &str) -> ByteSpan {
        let Expr::Call { func_span, span, .. } = &call else {
            panic!("eval needs a call expression");
        };
        let (func_span, span) = (*func_span, *span);
        self.call_site(file, Some(owner), span, func_span, callee);
        self.flow(
            file,
            FlowFact::Eval {
                scope: Scope::Decl(owner.decl),
                call,
            },
        );
        func_span
    }

    pub fn call_site(
        &mut self,
        file: usize,
        owner: Option<D>,
        span: ByteSpan,
        callee_span: ByteSpan,
        callee: &str,
    ) {
        // Member access separators of every language: `.`, PHP/C `->`, C++/Rust/PHP `::`.
        let normalized = callee.replace("->", ".").replace("::", ".");
        let member = normalized
            .rsplit('.')
            .next()
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        let receiver = normalized
            .rsplit_once('.')
            .and_then(|(head, _)| head.rsplit('.').next())
            .filter(|r| !["self", "cls", "this", "$this"].contains(r))
            .map(str::to_string);
        self.files[file].facts.calls.push(CallSite {
            owner: owner.map(|d| d.decl),
            lexical_owner: owner.map(|d| d.decl),
            span,
            callee_span,
            callee: callee.into(),
            member,
            receiver,
            line: 1,
            activation: Activation::Plain,
            is_new: false,
            arg_count: 0,
        });
    }

    /// Identity guards (`CallDetail::not_identical`) of the call at `span`; call details are
    /// filled up to stay aligned with the calls.
    pub fn guard(&mut self, file: usize, span: ByteSpan, guards: Vec<Expr>) {
        let facts = &mut self.files[file].facts;
        while facts.call_details.len() < facts.calls.len() {
            let call = facts.call_details.len() as u32;
            facts.call_details.push(CallDetail {
                call,
                receiver: None,
                arguments: Vec::new(),
                callee_path: None,
                not_identical: Vec::new(),
            });
        }
        let i = facts.calls.iter().position(|c| c.span == span).expect("call site");
        facts.call_details[i].not_identical = guards;
    }

    /// Receiver expression (`CallDetail::receiver`) of the call whose callee span is `at`;
    /// call details are filled up to stay aligned with the calls.
    pub fn call_receiver(&mut self, file: usize, at: ByteSpan, receiver: Expr) {
        let facts = &mut self.files[file].facts;
        while facts.call_details.len() < facts.calls.len() {
            let call = facts.call_details.len() as u32;
            facts.call_details.push(CallDetail {
                call,
                receiver: None,
                arguments: Vec::new(),
                callee_path: None,
                not_identical: Vec::new(),
            });
        }
        let i = facts
            .calls
            .iter()
            .position(|c| c.callee_span == at)
            .expect("call site");
        facts.call_details[i].receiver = Some(receiver);
    }

    pub fn implicit(&mut self, file: usize, owner: D, kind: ImplicitKind, subject: Expr) -> ByteSpan {
        let span = subject.span().unwrap_or_else(|| self.span());
        self.files[file].facts.implicit.push(ImplicitOp {
            scope: Scope::Decl(owner.decl),
            kind,
            subject,
            span,
            line: 1,
        });
        span
    }

    pub fn edge(&mut self, file: usize, owner: D, target: D, kind: EdgeKind, at: ByteSpan, line: u32) {
        let uid = self.uid(target);
        self.files[file].semantic.edges.push(SemEdge {
            owner: owner.decl,
            target: uid,
            kind,
            at,
            line,
            resolution: Resolution::CallHierarchy,
        });
    }

    pub fn unresolved(&mut self, file: usize, owner: D, at: ByteSpan, line: u32, callee: &str) {
        self.files[file].semantic.unresolved.push(SemUnresolved {
            owner: Some(owner.decl),
            kind: UnresolvedKind::NoSemanticTarget,
            at,
            line,
            callee: callee.into(),
            candidates: Vec::new(),
        });
    }

    /// Syntax reference `name` of kind `kind` owned by `owner` at a fresh span; returns it.
    pub fn reference(
        &mut self,
        file: usize,
        owner: Option<D>,
        name: &str,
        kind: trace_core::facts::RefKind,
    ) -> ByteSpan {
        let span = self.span();
        self.files[file].facts.references.push(trace_core::facts::Reference {
            span,
            name: name.into(),
            owner: owner.map(|d| d.decl),
            in_decorator: false,
            local: false,
            kind,
        });
        span
    }

    /// Syntax reference at an explicit span (header type references, bounds).
    pub fn reference_at(
        &mut self,
        file: usize,
        owner: Option<D>,
        name: &str,
        kind: trace_core::facts::RefKind,
        span: ByteSpan,
    ) {
        let facts = &mut self.files[file].facts;
        facts.references.push(trace_core::facts::Reference {
            span,
            name: name.into(),
            owner: owner.map(|d| d.decl),
            in_decorator: false,
            local: false,
            kind,
        });
        facts.references.sort_by_key(|r| (r.span.start, r.span.end));
    }

    /// Declared / constructed / annotated type of a subject (`FileFacts::types`).
    pub fn type_fact(
        &mut self,
        file: usize,
        subject: TypeSubject,
        type_name: &str,
        span: ByteSpan,
        source: TypeSource,
    ) {
        let facts = &mut self.files[file].facts;
        facts.types.push(TypeFact {
            subject,
            type_name: type_name.into(),
            span,
            source,
        });
        facts.types.sort_by_key(|t| (t.span.start, t.span.end));
    }

    /// Server-reported implementation of `base` (declared in `file`) by `implementor`
    /// (`FileSemantics::implementations`, recorded on the base file).
    pub fn implementation(&mut self, file: usize, base: D, implementor: D, kind: EdgeKind) {
        let uid = self.uid(implementor);
        self.files[file].semantic.implementations.push(SemImplementation {
            base: base.decl,
            implementor: uid,
            kind,
        });
    }

    /// `@decorators def function` bound as a variable of `scope` (a decorated nested
    /// function), decorators outermost first; decorator calls also get syntax call sites
    /// owned by `scope` (`callees`: one callee text per decorator, `None` for non-calls).
    pub fn decorated_in(
        &mut self,
        file: usize,
        scope: D,
        function: D,
        name: &str,
        decorators: Vec<(Expr, Option<&str>)>,
    ) -> Vec<ByteSpan> {
        let mut spans = Vec::new();
        let mut exprs = Vec::new();
        for (d, callee) in decorators {
            if let (Expr::Call { func_span, span, .. }, Some(callee)) = (&d, callee) {
                let (func_span, span) = (*func_span, *span);
                self.call_site(file, Some(scope), span, func_span, callee);
                spans.push(func_span);
            }
            exprs.push(d);
        }
        self.flow(
            file,
            FlowFact::Decorated {
                scope: Scope::Decl(scope.decl),
                target: BindTarget::Var {
                    scope: Scope::Decl(scope.decl),
                    name: name.into(),
                },
                function: function.decl,
                decorators: exprs,
            },
        );
        spans
    }

    pub fn callback(&mut self, file: usize, arg: CallbackArg) {
        self.files[file].facts.callbacks.push(arg);
    }

    /// The server answered the call whose callee span is `at` only with a declaration in an
    /// installed library file (`FileSemantics::library_calls`).
    pub fn library_call(&mut self, file: usize, at: ByteSpan, symbol: &str) {
        let language = self.files[file].language;
        let semantic = &mut self.files[file].semantic;
        let lib = trace_core::semantics::LibraryFile {
            path: format!("/site-packages/{symbol}.src"),
            package: symbol.split(['.', ':', '/']).next().unwrap_or(symbol).to_string(),
            version: Some("1.0".into()),
            stdlib: false,
            readable: true,
            language,
        };
        let idx = match semantic.library_files.iter().position(|f| *f == lib) {
            Some(i) => i,
            None => {
                semantic.library_files.push(lib);
                semantic.library_files.len() - 1
            }
        };
        semantic.library_calls.push(trace_core::semantics::SemLibraryCall {
            at,
            line: 1,
            file: idx as u32,
            decl_line: 1,
            decl_column: 0,
            symbol: Some(symbol.to_string()),
        });
    }

    pub fn impl_relation(&mut self, file: usize, type_name: &str, trait_name: &str) {
        let span = self.span();
        self.files[file].facts.impls.push(ImplRelation {
            type_name: type_name.into(),
            trait_name: trait_name.into(),
            span,
        });
    }

    pub fn build(&self) -> Index {
        let mut files: Vec<FileRecord> = self
            .files
            .iter()
            .map(|f| {
                let bytes = f.source.as_deref().unwrap_or("").as_bytes();
                FileRecord {
                    path: f.path.clone(),
                    language: f.language,
                    hash: Hash32::of(bytes),
                    size: bytes.len() as u64,
                    mtime_ns: 0,
                    support: SupportLevel::Semantic,
                    facts: Some(f.facts.clone()),
                    semantic: Some(if f.blind {
                        blind_semantics(f)
                    } else {
                        f.semantic.clone()
                    }),
                    first_symbol: 0,
                    symbol_count: 0,
                    diagnostics: Vec::new(),
                    pending: None,
                }
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        assemble(AssembleInput {
            header: IndexHeader {
                schema: trace_core::SCHEMA_VERSION,
                trace_version: trace_core::TRACE_VERSION.into(),
                root: self.root.clone(),
                built_unix: 0.0,
                syntax_version: 0,
                infer_version: crate::INFER_VERSION,
                bridge_version: 0,
                inventory_fingerprint: Hash32::default(),
                full_builds: 1,
                incremental_updates: 0,
            },
            files,
            configs: Vec::new(),
            omitted: Vec::new(),
            support: Vec::new(),
            backend_runs: Vec::new(),
            diagnostics: Vec::new(),
        })
    }
}
