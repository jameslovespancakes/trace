//! Per-language node-kind tables that drive ownership, laziness and value-flow lowering.
//!
//! Queries (`queries/<lang>.scm`) *find* things; these tables *classify* nodes while walking
//! the tree (which scope executes a call, what is lazy, how an expression lowers to `Expr`).
//! Every list holds tree-sitter node kind names of that grammar. Empty lists disable the
//! corresponding feature for the language (facts are then simply absent, never guessed).
//!
//! Child selectors ("picks") used in the tables are strings:
//!
//! | selector      | meaning                                                         |
//! |---------------|-----------------------------------------------------------------|
//! | `"field"`     | child in that field                                             |
//! | `"#0"`, `"#2"`| n-th named, non-extra child; `"#-1"` = last                     |
//! | `"=kind"`     | first named child of that kind                                  |
//! | `"a/b"`       | path: apply `a`, then `b` on the result                         |
//! | `"a\|b"`      | alternatives: the first selector that exists                    |
//! | `""`          | nothing (feature disabled)                                      |
//!
//! Picks select structure only (fields, positions, node kinds); they never look at source
//! text.

use trace_core::facts::ParamKind;
use trace_core::Language;

use crate::language_rules::{self, LanguageRules};

/// Static description of one grammar's relevant node kinds.
#[derive(Debug)]
pub struct SyntaxSpec {
    pub language: Language,
    /// The tree-sitter grammar (pinned grammar crate, see [`crate::grammar`]).
    pub grammar: fn() -> tree_sitter::Language,
    /// The tags-style query `queries/<lang>.scm` (capture contract SPEC §6.2).
    pub query: &'static str,

    // ---- ownership -------------------------------------------------------------------
    /// Nodes whose bodies execute only when *they* are called (not when the enclosing scope
    /// runs): function/method declarations, lambdas, arrow functions, closures, blocks passed
    /// as callbacks. Calls inside are owned by the declaration if it is a symbol, else `None`.
    pub lazy_scopes: &'static [&'static str],
    /// Anonymous callables (subset of `lazy_scopes`) modelled as synthetic `<lambda>`
    /// declarations: their bodies are owned by the synthetic declaration, they lower to
    /// `Expr::Lambda { function: Some(decl) }` and return their expression body. Anonymous
    /// lazy scopes not listed here stay owner-less (`None`).
    pub anonymous_functions: &'static [&'static str],
    /// Nodes whose *first* clause iterable runs eagerly and the rest lazily
    /// (Python generator expressions: only the first iterable is evaluated). They always
    /// become synthetic `<genexpr>` declarations owning the lazy part.
    pub generator_expressions: &'static [&'static str],
    /// The clause of a generator expression: `(kind, target field, iterable field)`.
    pub generator_clause: FieldPair,
    /// Class-like bodies of *anonymous* types: calls directly inside run at definition time
    /// (owner `None`). Bodies of declared classes are handled through the `@body` capture.
    pub class_bodies: &'static [&'static str],

    // ---- names -----------------------------------------------------------------------
    /// Identifier kinds that denote values (references, `Expr::Name`).
    pub identifiers: &'static [&'static str],
    /// Further identifier-like kinds (type/field/property identifiers); counted in
    /// `Declaration::identifiers` and accepted as member names, never value references.
    pub name_kinds: &'static [&'static str],
    /// Receiver expression kinds (`this`, `self`) lowered to `Expr::Name` with their text.
    pub self_kinds: &'static [&'static str],
    /// Receiver names never treated as a receiver binding (`self`, `cls`, `this`).
    pub self_names: &'static [&'static str],
    /// Constructor method names for class-call lookups (`__init__`, `constructor`).
    pub constructor_names: &'static [&'static str],

    // ---- expressions -----------------------------------------------------------------
    /// Member access `(object, field)` node kinds and their picks.
    pub member_access: &'static [MemberAccess],
    /// Call node kinds and the picks of callee / arguments.
    pub calls: &'static [CallShape],
    /// Argument wrapper kinds (C# `argument`): a `name` field makes
    /// a keyword argument; the value is the last named child.
    pub argument_wrappers: &'static [&'static str],
    /// Spread / splat argument kinds (`*args`, `...rest`); the operand is the first named child.
    pub spreads: &'static [&'static str],
    /// Keyword-unpacking kinds among `spreads` (`**kwargs`).
    pub keyword_spreads: &'static [&'static str],
    /// Keyword-argument node kinds (`keyword_argument`) with name/value picks.
    pub keyword_arguments: &'static [FieldPair],
    /// Conditional / boolean-choice expressions lowered to `Expr::Choice`.
    pub choices: &'static [ChoiceShape],
    /// Await expressions (operand = first named child).
    pub awaits: &'static [&'static str],
    /// Transparent wrappers lowered as their selected child (parentheses, casts, `x!`).
    pub unwrap: &'static [Unwrap],
    /// Subscript expressions `(kind, object pick)`.
    pub subscripts: &'static [FieldPair],
    /// Expression-list kinds whose elements are assigned pairwise (Go `a, b = x, y`).
    pub lists: &'static [&'static str],

    // ---- statements ------------------------------------------------------------------
    /// Assignment node kinds with target/value picks.
    pub assignments: &'static [FieldPair],
    /// Binding positions `(kind, field)`: the subtree of that child binds names (targets of
    /// assignments, loop variables, keyword names). Member/subscript targets bind only their
    /// attribute; their objects and indices are loads.
    pub store_fields: &'static [FieldPair],
    /// Subtrees whose identifiers are never value references (imports, parameter lists,
    /// annotations that are not values).
    pub binding_kinds: &'static [&'static str],
    /// `(kind, field)` children that are loads again inside a binding subtree (defaults).
    pub load_fields: &'static [FieldPair],
    /// Return statements (value = first named child).
    pub returns: &'static [&'static str],
    /// Loops over an iterable (Iterate activation / implicit ops).
    pub for_loops: &'static [ForLoop],
    /// Delegating yields (`yield from`, `yield*`): the operand is iterated.
    pub delegating_yields: &'static [TokenShape],
    /// Parents that iterate a direct child call (JS spread).
    pub iterate_parents: &'static [&'static str],
    /// `with` items (context expression pick).
    pub with_items: &'static [FieldPair],
    /// Delete statements (`del o[k]`).
    pub deletes: &'static [&'static str],
    /// Expression statements (result ignored).
    pub expression_statements: &'static [&'static str],
    /// Conditionals `(kind, condition pick)` (truthiness tests).
    pub conditions: &'static [FieldPair],
    /// Comparison expression kinds.
    pub comparisons: &'static [&'static str],
    /// Arithmetic / concatenation expression kinds.
    pub arithmetic: &'static [&'static str],
    /// Binary expressions classified by their operator token (`==` compares, `+` computes).
    pub binary_ops: &'static [&'static str],
    /// String interpolation holes (f-strings, template substitutions).
    pub interpolations: &'static [&'static str],

    // ---- declarations ----------------------------------------------------------------
    /// Comment node kinds (doc comments, leading comments).
    pub comments: &'static [&'static str],
    /// Wrappers widening a declaration span when the declaration is their only child of
    /// that kind (`export`, `decorated_definition`, `template<...>`, single `const f = ...`).
    pub wrappers: &'static [&'static str],
    /// Attribute nodes that precede a declaration as siblings (Rust `#[...]`, TS decorators
    /// in class bodies); they widen the span and become decorators.
    pub leading_attributes: &'static [&'static str],
    /// Non-symbol containers contributing to qualified names `(kind, name pick)`.
    pub namespaces: &'static [FieldPair],
    /// Import statement kinds (bindings read structurally by `crate::names`).
    pub imports: &'static [&'static str],
    /// Names visible without a binding (language builtins/globals); a dotted name rooted at
    /// one of them resolves to `<builtins_module>.<name>...` unless shadowed.
    pub builtins: &'static [&'static str],
    pub builtins_module: &'static str,
    /// Parameter node rules.
    pub params: &'static [ParamRule],
    /// Picks of the parameter list on the callable node.
    pub param_fields: &'static [&'static str],
    /// Parameter-list kinds searched in the declaration header when `param_fields` fail
    /// (C/C++ declarators).
    pub param_lists: &'static [&'static str],
    /// Separators after which parameters are keyword-only (Python `*`).
    pub keyword_separators: &'static [&'static str],
    /// How methods receive their instance (flow `ImplicitSelf`).
    pub receiver: Receiver,
    /// Decorators wrap the decorated function value (Python).
    pub decorators_wrap: bool,
    /// Callables without a body are stubs (prototypes, interface/abstract methods).
    pub stub_when_bodiless: bool,
    /// Emit implicit data-model operations (Python only).
    pub implicit_ops: bool,
    /// Statements declaring that their identifiers belong to an outer scope (Python
    /// `global` / `nonlocal`): the names are not variables of the current scope.
    pub scope_declarations: &'static [&'static str],
    /// Identity guards narrowing a bare-name callee (Python `is` / `is not`), see
    /// [`crate::guards`].
    pub identity_guards: bool,

    // ---- non-call references (extractor 5, SPEC §6.3 `RefKind`) -----------------------
    /// Store-position parents (kinds of `store_fields` entries) that *declare* new local
    /// names (`let x`, `const x`, `var x`, loop variables, named-argument labels): bare
    /// identifiers bound there get no `write` reference. Attribute targets are always writes.
    pub declaring_stores: &'static [&'static str],
    /// Store positions selected by an operator token (R `x <- v`, `v -> x`).
    pub operator_stores: &'static [OperatorStore],
    /// Node kinds whose subtree is a type position (annotations, `extends` / `implements`,
    /// generic arguments): identifiers and name kinds inside are `type` references.
    pub type_contexts: &'static [&'static str],
    /// Field names whose child is a type position (`type`, `return_type`).
    pub type_fields: &'static [&'static str],
    /// Type-name node kinds (`type_identifier`) recorded as `type` references wherever they
    /// occur outside binding positions (declared names are never references).
    pub type_names: &'static [&'static str],
    /// Re-export list entries `(kind, name field, alias field)` (JS/TS `export_specifier`):
    /// the name is an `export` reference, the alias is a label.
    pub export_specifiers: &'static [FieldPair],
    /// Node kinds that may bind an import through a loader call (`const m = require("m")`,
    /// Bash `source x`, R `library(x)`). They are read by
    /// `crate::names::read_call_imports`; unlike `imports` the subtree stays ordinary code.
    pub import_calls: &'static [&'static str],
    /// Literal expressions that allocate a value of the named type without a call (Go
    /// composite literals `T{..}`): `(kind, type field, "")`. Lowered like `new T()`
    /// (`Expr::Call { func: Name(T), is_new: true }`) when the type is a (qualified)
    /// identifier; slice / map / array literals stay opaque.
    pub literal_allocations: &'static [FieldPair],

    // ---- callback argument forms (DESIGN §1.10 item 1) ------------------------------
    /// Argument forms passing a function that are neither a plain name nor a member access:
    /// method / callable references (`X::m`, `::f`), address-of (`&f`), scoped paths
    /// (`module::f`), symbols naming a method (`&:sym`, `method(:x)`), captures (`&f/1`),
    /// first-class callable syntax (`f(...)`).
    pub callback_forms: &'static [CallbackForm],
    /// A member access without parentheses evaluates (calls) the member (Scala
    /// parameterless methods `x.y`): a member-access argument passes the member's value,
    /// never a function reference. Only plain names, callback forms and member accesses on
    /// a placeholder stay callback arguments.
    pub member_values_are_calls: bool,
    /// Placeholder parameter kinds (Scala `_`): a member access on one is an anonymous
    /// function (`_.name`) and stays a callback argument.
    pub placeholders: &'static [&'static str],

    // ---- calls that are not calls, calls by name (language rules) ---------------------
    /// Callee kinds that are types by grammar (C++ functional cast `int(x)`:
    /// `primitive_type`): the call converts a value and runs no function, so it is no call
    /// site.
    pub type_callees: &'static [&'static str],
    /// Dereference forms naming a pointer type when parenthesized as a callee (Go
    /// `(*T)(x)`): `(dereference kind, operand pick, operator token)`. Where calling a type
    /// converts ([`type_call_is_conversion`]) such a call is a conversion to the pointer
    /// type unless the operand's root name is a local value (a pointer to a function); the
    /// type name becomes a `type` reference.
    pub pointer_conversions: &'static [FieldPair],
    /// Calls that call the function / method named by a literal argument (R
    /// `do.call("f", args)`): an additional call site of that name.
    pub name_calls: &'static [NameCall],
    /// Named separator kinds inside argument lists (R `comma`): never arguments.
    pub separators: &'static [&'static str],

    // ---- declarations by language rule -----------------------------------------------
    /// Function-like macro definitions (C / C++ `#define F(x) ...`): callable declarations
    /// that are never stubs; their replacement text is no executing code.
    pub macro_definitions: &'static [&'static str],
    /// Callee names that make the calling function a generic whose implementations are
    /// found by naming convention (R `UseMethod("g")`): the function is declared as a stub
    /// (`is_stub`), its methods `g.<class>` implement it (dispatch rule `r-s3`).
    pub generic_dispatch_calls: &'static [&'static str],
    // ---- declared and constructed types (`crate::typefacts`) ---------------------------
    /// Typed binding forms (parameters, variables, fields) and their picks.
    pub(crate) type_forms: &'static [crate::typefacts::Form],
    /// Return-type positions: `(callable node kind, type pick)`.
    pub(crate) return_types: &'static [(&'static str, &'static str)],
    /// Constructor calls are plain call syntax (`Foo(...)`).
    pub(crate) constructs_by_call: bool,
    /// Qualification separator of type paths.
    pub(crate) type_path_separator: &'static str,
    // ---- lexical scopes (`crate::scopes`) ----------------------------------------------
    /// Scoping rules (Python has none: it uses `crate::names::Names`).
    pub(crate) scopes: &'static crate::scopes::ScopeRules,
    // ---- value-flow lowering (`crate::lower`) ------------------------------------------
    /// Object literals whose keyed entries are members.
    pub(crate) object_literals: &'static [&'static str],
    /// Non-delegating yield expressions; `yield from` / `yield*` iterate instead
    /// (`delegating_yields`).
    pub(crate) yields: &'static [&'static str],
    /// Objects are property bags at run time (JavaScript / TypeScript properties): a computed
    /// member store with a string key (`o["get"] = f`, `o[k] = f`) is the member store
    /// `o.get = f`, and a function defined under a member name (`o.m = function () {}`) is
    /// stored in that member.
    pub(crate) dynamic_members: bool,
    /// Loops the index lowering does not model (library mode only).
    pub(crate) library_loops: &'static [ForLoop],
    // ---- interface fingerprint (`crate::interface`) ------------------------------------
    /// How return types are known to dependents.
    pub(crate) return_rule: crate::interface::ReturnRule,
    // ---- imports (`crate::names`) -------------------------------------------------------
    /// Import statement readers `(node kind, reader)`.
    pub(crate) import_readers: &'static [(&'static str, crate::names::ImportReader)],
    /// Reader of loader calls binding a module (`import_calls` kinds: JS `require`, R
    /// `library`, Bash `source`).
    pub(crate) call_import_reader: Option<crate::names::ImportReader>,
    // ---- library tables ------------------------------------------------------------------
    /// Key of the embedded library table (`assets/library/<key>.json`; JS / TS / TSX share
    /// `javascript`).
    pub(crate) library_table: Option<&'static str>,
    // ---- call-site views (`crate::callsite`) ------------------------------------------
    /// Empty case clauses fall through into the next one.
    pub(crate) cases_fall_through: bool,
    // ---- language rules ----------------------------------------------------------------
    /// Calling a type converts a value ([`type_call_is_conversion`]).
    pub(crate) type_calls_convert: bool,
    /// [`SyntaxSpec::name_calls`]).

    // ---- inference ------------------------------------------------------------------
    /// Language rules of names, modules, inheritance, dispatch and types (trace-infer).
    pub rules: LanguageRules,
}

/// A call that calls the function named by one of its arguments (see
/// [`SyntaxSpec::name_calls`]).
#[derive(Debug, Clone, Copy)]
pub struct NameCall {
    /// Member name of the calling function (`send`, `do.call`).
    pub member: &'static str,
    /// Positional index of the argument naming the called function.
    pub position: u32,
    /// Keyword of the same argument (`what`); empty = none.
    pub keyword: &'static str,
    /// Accepted argument kinds: symbol and string literals, and plain names where
    /// functions are values (R `do.call(f, args)`).
    pub kinds: &'static [&'static str],
}

/// One callback argument form (see [`SyntaxSpec::callback_forms`]).
#[derive(Debug, Clone, Copy)]
pub struct CallbackForm {
    /// Argument node kind (the argument list child, or its value after unwrapping keyword /
    /// wrapper / spread nodes).
    pub kind: &'static str,
    /// Pick selector of the node naming the function inside the argument.
    pub name: &'static str,
    /// Node kinds the picked name node must have.
    pub name_kinds: &'static [&'static str],
    /// Minimum number of named children of the argument node (`X::m` has two, `X::new`
    /// only one).
    pub min_named: usize,
    /// `(pick selector, exact text)` that must hold (operator token, helper method name);
    /// `("", "")` = none.
    pub guard: (&'static str, &'static str),
}

pub(crate) const fn callback_form(
    kind: &'static str,
    name: &'static str,
    name_kinds: &'static [&'static str],
    min_named: usize,
    guard: (&'static str, &'static str),
) -> CallbackForm {
    CallbackForm {
        kind,
        name,
        name_kinds,
        min_named,
        guard,
    }
}

/// A store position selected by an operator token.
#[derive(Debug, Clone, Copy)]
pub struct OperatorStore {
    pub kind: &'static str,
    /// The stored-to child.
    pub pick: &'static str,
    /// Operator tokens (anonymous children) that make it a store.
    pub operators: &'static [&'static str],
}

/// A call node kind with its callee/arguments picks.
#[derive(Debug, Clone, Copy)]
pub struct CallShape {
    pub kind: &'static str,
    /// The callee (function expression, or the method name when `receiver_field` is set).
    pub function_field: &'static str,
    pub arguments_field: &'static str,
    /// Receiver pick for calls whose callee is split into receiver + name
    /// (Java `obj.m()`): the callee span then runs from the receiver start.
    pub receiver_field: &'static str,
    /// `new` expression (constructor call syntax).
    pub is_new: bool,
}

/// Member access node kind with object/property picks.
#[derive(Debug, Clone, Copy)]
pub struct MemberAccess {
    pub kind: &'static str,
    pub object_field: &'static str,
    pub property_field: &'static str,
}

/// A node kind with two relevant picks (left/right, name/value, value/-, ...).
#[derive(Debug, Clone, Copy)]
pub struct FieldPair {
    pub kind: &'static str,
    pub first: &'static str,
    pub second: &'static str,
}

/// A choice expression: alternatives by pick; `operators` restricts to operator tokens.
#[derive(Debug, Clone, Copy)]
pub struct ChoiceShape {
    pub kind: &'static str,
    pub alternatives: &'static [&'static str],
    /// Required operator token kinds (anonymous children); empty = any.
    pub operators: &'static [&'static str],
}

/// A transparent wrapper node lowered as its selected child.
#[derive(Debug, Clone, Copy)]
pub struct Unwrap {
    pub kind: &'static str,
    pub pick: &'static str,
}

/// A loop over an iterable.
#[derive(Debug, Clone, Copy)]
pub struct ForLoop {
    pub kind: &'static str,
    pub target: &'static str,
    pub iterable: &'static str,
    /// Required anonymous token child (JS `of`); empty = none.
    pub token: &'static str,
}

/// A node kind qualified by an anonymous token child (`yield from`, `yield*`).
#[derive(Debug, Clone, Copy)]
pub struct TokenShape {
    pub kind: &'static str,
    pub token: &'static str,
}

/// Where a parameter node keeps its name.
#[derive(Debug, Clone, Copy)]
pub enum NameAt {
    /// The node itself is the name.
    Itself,
    /// A pick (field, position, kind).
    Pick(&'static str),
    /// Not a parameter (TS `this` parameter).
    Skip,
}

/// How to read one parameter node kind.
#[derive(Debug, Clone, Copy)]
pub struct ParamRule {
    pub kind: &'static str,
    pub name: NameAt,
    pub param_kind: ParamKind,
    /// Pick of the default value (empty = none).
    pub default: &'static str,
}

/// How methods receive their instance, for flow `ImplicitSelf` facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver {
    None,
    /// The first positional parameter of a method lexically inside a class (Python `self`).
    FirstParam,
    /// An implicit receiver name inside class methods (`this`, `self`, `$this`).
    Implicit(&'static str),
    /// A parameter with this name receives the container type (Rust `self`).
    SelfParam(&'static str),
    /// The receiver list in this field names the parameter (Go `func (s *T) m()`).
    ReceiverField(&'static str),
}

pub(crate) const fn fp(kind: &'static str, first: &'static str, second: &'static str) -> FieldPair {
    FieldPair { kind, first, second }
}

pub(crate) const fn call(
    kind: &'static str,
    function_field: &'static str,
    arguments_field: &'static str,
) -> CallShape {
    CallShape {
        kind,
        function_field,
        arguments_field,
        receiver_field: "",
        is_new: false,
    }
}

pub(crate) const fn method_call(
    kind: &'static str,
    name: &'static str,
    arguments: &'static str,
    receiver: &'static str,
) -> CallShape {
    CallShape {
        kind,
        function_field: name,
        arguments_field: arguments,
        receiver_field: receiver,
        is_new: false,
    }
}

pub(crate) const fn new_call(
    kind: &'static str,
    function_field: &'static str,
    arguments_field: &'static str,
) -> CallShape {
    CallShape {
        kind,
        function_field,
        arguments_field,
        receiver_field: "",
        is_new: true,
    }
}

pub(crate) const fn member(kind: &'static str, object: &'static str, property: &'static str) -> MemberAccess {
    MemberAccess {
        kind,
        object_field: object,
        property_field: property,
    }
}

pub(crate) const fn unwrap(kind: &'static str, pick: &'static str) -> Unwrap {
    Unwrap { kind, pick }
}

pub(crate) const fn param(
    kind: &'static str,
    name: NameAt,
    param_kind: ParamKind,
    default: &'static str,
) -> ParamRule {
    ParamRule {
        kind,
        name,
        param_kind,
        default,
    }
}

pub(crate) const fn for_loop(kind: &'static str, target: &'static str, iterable: &'static str) -> ForLoop {
    ForLoop {
        kind,
        target,
        iterable,
        token: "",
    }
}

pub(crate) const fn choice(kind: &'static str, alternatives: &'static [&'static str]) -> ChoiceShape {
    ChoiceShape {
        kind,
        alternatives,
        operators: &[],
    }
}

impl SyntaxSpec {
    /// All-empty tables of `language` with its grammar and query; every language file
    /// overrides what its grammar supports.
    pub(crate) const fn empty(
        language: Language,
        grammar: fn() -> tree_sitter::Language,
        query: &'static str,
    ) -> SyntaxSpec {
        SyntaxSpec {
            language,
            grammar,
            query,
            lazy_scopes: &[],
            anonymous_functions: &[],
            generator_expressions: &[],
            generator_clause: fp("", "", ""),
            class_bodies: &[],
            identifiers: &[],
            name_kinds: &[],
            self_kinds: &[],
            self_names: &[],
            constructor_names: &[],
            member_access: &[],
            calls: &[],
            argument_wrappers: &[],
            spreads: &[],
            keyword_spreads: &[],
            keyword_arguments: &[],
            choices: &[],
            awaits: &[],
            unwrap: &[],
            subscripts: &[],
            lists: &[],
            assignments: &[],
            store_fields: &[],
            binding_kinds: &[],
            load_fields: &[],
            returns: &[],
            for_loops: &[],
            delegating_yields: &[],
            iterate_parents: &[],
            with_items: &[],
            deletes: &[],
            expression_statements: &[],
            conditions: &[],
            comparisons: &[],
            arithmetic: &[],
            binary_ops: &[],
            interpolations: &[],
            comments: &[],
            wrappers: &[],
            leading_attributes: &[],
            namespaces: &[],
            imports: &[],
            builtins: &[],
            builtins_module: "",
            params: &[],
            param_fields: &[],
            param_lists: &[],
            keyword_separators: &[],
            receiver: Receiver::None,
            decorators_wrap: false,
            stub_when_bodiless: false,
            implicit_ops: false,
            scope_declarations: &[],
            identity_guards: false,
            declaring_stores: &[],
            operator_stores: &[],
            type_contexts: &[],
            type_fields: &[],
            type_names: &[],
            export_specifiers: &[],
            import_calls: &[],
            literal_allocations: &[],
            callback_forms: &[],
            member_values_are_calls: false,
            placeholders: &[],
            type_callees: &[],
            pointer_conversions: &[],
            name_calls: &[],
            separators: &[],
            macro_definitions: &[],
            generic_dispatch_calls: &[],
            type_forms: &[],
            return_types: &[],
            constructs_by_call: false,
            type_path_separator: ".",
            scopes: &crate::scopes::NONE,
            object_literals: &[],
            yields: &[],
            dynamic_members: false,
            library_loops: &[],
            return_rule: crate::interface::ReturnRule::Declared,
            import_readers: &[],
            call_import_reader: None,
            library_table: None,
            cases_fall_through: false,
            type_calls_convert: false,
            rules: language_rules::NONE,
        }
    }
}

/// Language rule: calling a type converts a value (`T(x)` in Go); no constructor runs, so the
/// call reaches no declaration of the type.
pub fn type_call_is_conversion(language: Language) -> bool {
    crate::languages::syntax(language).is_some_and(|s| s.type_calls_convert)
}

impl SyntaxSpec {
    #[inline]
    pub fn is_identifier(&self, kind: &str) -> bool {
        self.identifiers.contains(&kind)
    }

    /// Identifier-like kinds (values, members, types).
    #[inline]
    pub fn is_name_like(&self, kind: &str) -> bool {
        self.identifiers.contains(&kind) || self.name_kinds.contains(&kind)
    }

    #[inline]
    pub fn member(&self, kind: &str) -> Option<&MemberAccess> {
        self.member_access.iter().find(|m| m.kind == kind)
    }

    #[inline]
    pub fn call_shape(&self, kind: &str) -> Option<&CallShape> {
        self.calls.iter().find(|c| c.kind == kind)
    }

    #[inline]
    pub fn unwrap_pick(&self, kind: &str) -> Option<&'static str> {
        self.unwrap.iter().find(|u| u.kind == kind).map(|u| u.pick)
    }

    #[inline]
    pub fn is_lazy(&self, kind: &str) -> bool {
        self.lazy_scopes.contains(&kind)
    }

    #[inline]
    pub fn is_comment(&self, kind: &str) -> bool {
        self.comments.contains(&kind)
    }

    #[inline]
    pub fn assignment(&self, kind: &str) -> Option<&FieldPair> {
        self.assignments.iter().find(|a| a.kind == kind)
    }

    #[inline]
    pub fn subscript(&self, kind: &str) -> Option<&FieldPair> {
        self.subscripts.iter().find(|s| s.kind == kind)
    }

    #[inline]
    pub fn param_rule(&self, kind: &str) -> Option<&ParamRule> {
        self.params.iter().find(|p| p.kind == kind)
    }
}

#[cfg(test)]
#[path = "../tests/unit/spec.rs"]
mod tests;
