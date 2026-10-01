//! Generic, flow-insensitive value flow with allocation-site and receiver sensitivity,
//! anonymous scopes, library behaviour and test provenance.
//!
//! One mechanism, not per-pattern rules: function/class/instance values — seeded only by
//! semantically resolved value references (`Index::value_refs`), proven call edges and
//! anonymous scopes (lambdas, generator expressions) — propagate through variables,
//! parameters (arguments and defaults), attributes, class members, decorators and returns
//! until a fixpoint. Unresolved calls, argument consumption and implicit operations then ask
//! which values reach them.
//!
//! ## Abstract domain
//!
//! * `Function(f)` — a function, lambda or unbound method value;
//! * `Class(c)`;
//! * `Instance(obj)` — an abstract object `obj = (class, alloc)`: every construction `C(...)`
//!   and every class-decorator application `@D def f` is its own allocation (`At(file,
//!   byte)`); `Any` stands for an instance of unknown allocation;
//! * `Bound(f, recv)` — a method bound to a receiver (`obj.m`, `cls.m` for classmethods);
//! * `Generator(g)` — the generator object of a generator expression (its body runs only
//!   when the object is consumed);
//! * `Property(f)` — a property-like descriptor (`property(f)`, `functools.cached_property`)
//!   whose getter runs on instance attribute access;
//! * `Object(alloc)` — a plain object without a class: an object / table literal
//!   (`{a: f}`) or the result of a library call that creates an object delegating
//!   to another (`Object.create(p)`); its members are per-allocation property slots;
//! * `Library(l)` — an object a library created (the result of a call the server resolved
//!   into library code, `l` = that library symbol), unless the library knowledge says the
//!   call returns something else (`returns`, `wraps`, ...). Its members are library members
//!   (reading one gives the same library object) unless repository code stored them.
//!
//! Function values and library objects carry properties too (`app.handle = ...` on a
//! function object). Member copying and delegation (library `copies_members` /
//! `delegates_members` effects: mixins, `Object.create`, `Object.setPrototypeOf`; the JavaScript `__proto__` store and `__proto__` literal
//! key are language rules) add *delegates* to an object: a member lookup that finds nothing
//! on the object itself continues in its delegates (at most `flow.max_delegate_visits` objects
//! per lookup; reaching the bound marks the candidates `bounded`). JavaScript
//! `C.prototype` of a class is the class's instance side (prototype members are members of
//! every instance).
//!
//! ## Receiver sensitivity (1-object / cls-sensitive)
//!
//! Methods (callables with an `ImplicitSelf` fact) are analysed once per *receiver context*:
//! * the declared context — exactly the declaring class (`Instance(C, Any)` / `Class(C)`),
//!   always present, so every method is analysed as a query root;
//! * every concrete receiver flowing from a call site (`Bound(f, recv)`, constructor calls
//!   binding the new object, implicit data-model calls binding the operand, `super()`);
//! * `Unknown` when some call reaches the method with a receiver that evaluates to nothing;
//!   the receiver parameter is then the whole class family (the reference design's rule).
//!
//! Variables and returns of methods are keyed by context; at most `flow.max_contexts` contexts
//! per method, further receivers are widened to their class (`Any` allocation).
//!
//! ## Slots
//!
//! `Var(scope, ctx, name)`, `Default(fn, name)` (parameter defaults, all contexts),
//! `Member(class, name)`, `Field(name)` (field-only evidence), `ObjAttr(obj, name)`
//! (per-allocation attribute slots), `ClassAttr(class, name)`, `Return(fn, ctx)`,
//! `Prop(holder, name)` (properties of plain objects, function objects and library
//! objects), `Delegates(holder)` (objects a member lookup continues in).
//!
//! ## eval(expr)
//!
//! * `Name`/`Attr` whose identifier span is a value ref -> `Function|Class(target)`; method
//!   targets are bound to the evaluated receiver objects (`Bound`);
//! * `Name` -> the receiver values if it is the method's receiver parameter, else
//!   `Var(scope, ctx, n) ∪ Default(fn, n) ∪ Var(module, n)`; anonymous scopes also see the
//!   variables of their enclosing scopes (closures);
//! * `Attr` -> member lookup on classes/instances (first class in the MRO with a member slot
//!   or method; methods are bound to the receiver; `Property` members run their getter),
//!   else per-allocation attributes (an `Any` object reads every allocation of its class),
//!   else class attributes; when all of that finds nothing, `Field(attr)` (field-name
//!   fallback, weak evidence) unless strict;
//! * `Call` -> apply callees (value flow ∪ proven targets in the callee span); when the callee
//!   has no in-index value, the library knowledge of that call (trace-library: derived from
//!   the installed library source, declared function types or the native table; every
//!   language, [`crate::behaviour`]) gives its behaviour; Python's zero-argument `super()`
//!   is a language rule (PEP 3135: the compiler supplies the class and the receiver);
//! * `Lambda` -> `Function(lambda)` or `Generator(genexpr)`; `Choice` -> union; `Await` ->
//!   inner; else ∅;
//! * decorators apply innermost first with the function value as the only argument (each
//!   application is an allocation); unresolved decorators pass the value through, library
//!   decorators apply their knowledge (`property` wraps it, a decorator factory's
//!   `Decorates` effect applies to the decorated function).
//!
//! ## Candidates ([`Flow::candidates`])
//!
//! * unresolved call (no proven target in the callee span) with non-empty callees: `flow`
//!   `call`, targets = functions ∪ constructors of classes ∪ `__call__` of instances;
//!   `field_only` = targets not found in strict mode (no `Field` slots); callee values are
//!   narrowed by identity guards (`CallDetail::not_identical`: a guard evaluating to exactly
//!   one function or class removes that value); when at least two product targets exist but
//!   every receiver context (product solution) yields at most one strong target, the
//!   candidate is `receiver_exact` (each target is *the* callee under some receiver);
//! * resolved method call `recv.m(...)`: overrides found by member lookup on the evaluated
//!   receivers; class-hierarchy (CHA) overrides only when the receiver is unknown or is the
//!   method's own receiver parameter in its declared context (virtual self-calls) — never
//!   for `super()` calls; minus the proven targets: `flow` `override_dispatch`;
//! * `super`: `super().m()` (Python language rule) and the super keyword as a receiver
//!   (`super.m()` in JS/TS/Java/Scala, C# `base.m()`, PHP `parent::m()`)
//!   look `m` up in the receiver's MRO *after* the calling method's declaring class, bound
//!   to the current receiver (so an override calling `super` reaches the base member, not
//!   itself); never CHA-widened;
//! * decorator calls (`@app.get("/")` in a `Decorated` fact) are calls of the decorated
//!   definition's scope: they get the same `flow` `call` candidates as statement calls
//!   (e.g. `app` a parameter injected by name whose value is a subclass instance: member lookup
//!   walks the subclass's MRO to the inherited base member);
//! * implicit ops (Python only): data-model methods of the operand objects, `__get__` of
//!   descriptor members, getters of `Property` members (minus proven targets at the op);
//! * argument consumption: an argument passed to a parameter that is *called* (directly,
//!   through a nested wrapper, or forwarded to another consuming parameter) or to a library
//!   position that runs it -> one `callback` candidate per function value (a library
//!   `calls_method` position: the named method of the passed objects); an argument
//!   passed to an *iterated* parameter / library position -> `implicit` `iterate` candidate
//!   (`__iter__`/`__next__` of objects, bodies of generator expressions) at the argument;
//! * composition by receiver: when a candidate method runs with a concrete receiver
//!   (allocation or class), the calls inside it through its receiver parameter
//!   (`self.func(...)`) are resolved under that receiver (strict) and reported as a composed
//!   candidate `via` the parent candidate — e.g. descriptor access resolves to the function
//!   stored in that allocation.
//!
//! Module-level code produces candidates only when its file has a synthetic `<module>`
//! declaration (`FileFacts::module_decl`, extractor 5), which then owns them. The solver's
//! treatment of module scopes is unchanged (no proven callees are looked up for them).
//!
//! ## Provenance (one solve, test-origin bits)
//!
//! The system is solved once in two phases over one state: first without test constraints
//! (test symbols, their nested scopes, test files), then — continuing from that fixpoint —
//! with them. Every stored value and receiver context carries a test-origin bit (set when it
//! was first derived in the second phase). The *product view* reads only values without the
//! bit (exactly the product-only fixpoint), the *full view* reads everything. Product-owned
//! candidates come from the product view; targets that appear only in the full view are
//! reported as `test_only` (possible tier, never decided). Test-owned sites use the full view.
//!
//! ## Solver (scale contract, SPEC §5.1a)
//!
//! Rounds over the constraints in order (the reference design's chaotic iteration, so
//! results match it), but a (constraint, receiver context) item is re-evaluated only when
//! one of the state cells it read during its last evaluation (slots, context sets,
//! allocation lists — recorded dynamically) changed since then. Unchanged constraints are
//! never re-evaluated; `flow.max_iterations` rounds per phase and a global evaluation budget
//! (`flow.eval_budget_per_constraint` × constraints + `flow.eval_budget_base`, or a fixed
//! `flow.eval_budget`; settings) are safety bounds only (`flow_bound` when hit).
//!
//! Bounds: every slot holds at most `flow.max_slot_values` values. Beyond that its allocations
//! are widened (`Instance(C, At)` -> `Instance(C, Any)`, bound receivers likewise); if it is
//! still too large it is *saturated* (further values are not stored). Allocations per
//! (class, attribute) are capped the same way (further ones write the `Any` object's slot).
//! Candidates computed from a saturated slot are marked [`FlowCandidate::bounded`] and the
//! solve reports a `flow_bound` diagnostic ([`Flow::diagnostics`]): nothing disappears
//! silently.
//!
//! Evaluation is read-only; bindings and new receiver contexts discovered while evaluating
//! one item are applied right after it (every transfer function is monotone up to the
//! bounds above).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rayon::prelude::*;
use trace_core::config::FlowSettings;
use trace_core::facts::{BindTarget, Expr, FlowFact, ImplicitKind, Scope};
use trace_core::model::{
    ByteSpan, Diagnostic, ExecutionModel, FileId, LibraryReceiver, Location, SiteOperation, Symbol, SymbolId,
};
use trace_core::{EdgeKind, Index, Language};

use trace_library::table::{IrreducibleRow, RowSel, Section};
use trace_library::{ArgSel, CallBehaviour, Effect, LibraryKnowledge};
use trace_syntax::lower::{LITERAL_PREFIX, OBJECT_CALLEE, YIELDED};

use crate::behaviour::{applied_effects, selects};
use trace_syntax::language_rules::rules;

use crate::hierarchy::{constructor_name, is_constructor, Hierarchy};
use crate::LibraryInputs;
use js_objects::{JsObjects, IMPLICIT_EXPORTS_ALLOC};

/// Python data-model methods per implicit operation.
pub(crate) fn data_model_methods(kind: ImplicitKind) -> &'static [&'static str] {
    match kind {
        ImplicitKind::SubscriptStore => &["__setitem__"],
        ImplicitKind::SubscriptLoad => &["__getitem__"],
        ImplicitKind::SubscriptDelete => &["__delitem__"],
        ImplicitKind::WithEnter => &["__enter__", "__exit__"],
        ImplicitKind::Iterate => &["__iter__"],
        ImplicitKind::DescriptorGet => &["__get__"],
    }
}

/// Site operation of an implicit data-model operation.
pub(crate) fn implicit_operation(kind: ImplicitKind) -> SiteOperation {
    match kind {
        ImplicitKind::SubscriptStore => SiteOperation::SubscriptStore,
        ImplicitKind::SubscriptLoad => SiteOperation::SubscriptLoad,
        ImplicitKind::SubscriptDelete => SiteOperation::SubscriptDelete,
        ImplicitKind::WithEnter => SiteOperation::WithEnter,
        ImplicitKind::Iterate => SiteOperation::Iterate,
        ImplicitKind::DescriptorGet => SiteOperation::DescriptorGet,
    }
}

/// Allocation site of an abstract object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Alloc {
    /// Unknown allocation (declared receiver contexts, widened contexts).
    Any,
    /// Construction / decoration at (file, callee or decorator start byte).
    At(FileId, u32),
}

/// An abstract object: instances of `class` allocated at `alloc`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Obj {
    pub class: SymbolId,
    pub alloc: Alloc,
}

impl Obj {
    pub const fn any(class: SymbolId) -> Obj {
        Obj {
            class,
            alloc: Alloc::Any,
        }
    }
}

/// Receiver context of a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Recv {
    /// Unknown receiver: the receiver parameter is the whole class family.
    Unknown,
    Class(SymbolId),
    Instance(Obj),
}

impl Recv {
    fn class(self) -> Option<SymbolId> {
        match self {
            Recv::Unknown => None,
            Recv::Class(c) => Some(c),
            Recv::Instance(o) => Some(o.class),
        }
    }

    /// A concrete receiver: a class, or an instance with a known allocation site.
    pub(crate) fn is_specific(self) -> bool {
        matches!(
            self,
            Recv::Class(_)
                | Recv::Instance(Obj {
                    alloc: Alloc::At(..),
                    ..
                })
        )
    }

    fn widen(self) -> Recv {
        match self {
            Recv::Instance(o) => Recv::Instance(Obj::any(o.class)),
            other => other,
        }
    }
}

/// A value in the abstract domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Value {
    Function(SymbolId),
    Class(SymbolId),
    Instance(Obj),
    Bound(SymbolId, Recv),
    Generator(SymbolId),
    Property(SymbolId),
    /// A plain object (object / table literal, library-made delegating object).
    Object(Alloc),
    /// An object created by a library (index into `Flow::libraries`: its library symbol).
    Library(u32),
}

/// Owner of property slots and delegates other than a class-based attribute slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Holder {
    /// A plain object.
    Obj(Alloc),
    /// A function object (`f.attr = v`).
    Fn(SymbolId),
    /// A library-created object (`Flow::libraries` index).
    Lib(u32),
    /// A class instance (delegates only: its attributes are `ObjAttr` slots).
    Inst(Obj),
}

impl Holder {
    /// The holder of a value's own properties / delegates, when it has one.
    fn of(v: Value) -> Option<Holder> {
        match v {
            Value::Object(a) => Some(Holder::Obj(a)),
            Value::Function(f) => Some(Holder::Fn(f)),
            Value::Library(l) => Some(Holder::Lib(l)),
            Value::Instance(o) => Some(Holder::Inst(o)),
            Value::Class(_) | Value::Bound(..) | Value::Generator(_) | Value::Property(_) => None,
        }
    }
}

impl Value {
    /// The value with its allocation forgotten (slot widening).
    fn widened(self) -> Value {
        match self {
            Value::Instance(o) => Value::Instance(Obj::any(o.class)),
            Value::Bound(f, r) => Value::Bound(f, r.widen()),
            other => other,
        }
    }
}

/// What kind of site a flow candidate proposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CandidateKind {
    /// Unresolved call or override dispatch at a call's callee span.
    Flow,
    /// Implicit data-model operation (operand span) or argument consumption (argument span).
    Implicit,
    /// A function value passed to a consuming parameter / library position; `arg` is the
    /// argument span (identifier span for names and attributes).
    Callback { arg: ByteSpan },
}

/// A flow-generated candidate set for one site.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowCandidate {
    pub owner: SymbolId,
    pub file: FileId,
    /// Callee span (calls, callbacks) or operand / argument span (implicit).
    pub span: ByteSpan,
    pub line: u32,
    /// Callee text when known from syntax (empty for implicit operations).
    pub callee: String,
    /// Sorted by uid; includes `field_only` and `test_only`.
    pub candidates: Vec<SymbolId>,
    pub field_only: Vec<SymbolId>,
    /// Targets reached only through values originating in test code.
    pub test_only: Vec<SymbolId>,
    pub operation: SiteOperation,
    pub kind: CandidateKind,
    /// Composed candidate: (index of the parent candidate in the returned list, the parent
    /// target this candidate is reached through).
    pub via: Option<(usize, SymbolId)>,
    /// Unresolved call with at least two product candidates where every receiver context of
    /// the owner (product solution) resolves the callee to at most one strong target.
    pub receiver_exact: bool,
    /// Computed from a saturated slot (`flow.max_slot_values`): the set may be incomplete.
    pub bounded: bool,
}

/// Work and size counters of one solve (`TRACE_PROFILE`, scale tests).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlowStats {
    /// Constraints compiled from flow facts.
    pub constraints: u64,
    /// (constraint, receiver context) items evaluated at least once.
    pub items: u64,
    /// Transfer-function evaluations by the solver.
    pub evals: u64,
    /// Item visits skipped because nothing they read had changed.
    pub skipped: u64,
    pub rounds_product: u64,
    pub rounds_full: u64,
    /// Slots holding at least one value, and the values they hold.
    pub slots: u64,
    pub values: u64,
    /// Receiver contexts (all methods).
    pub contexts: u64,
    pub widened_slots: u64,
    pub saturated_slots: u64,
    /// Attribute writes of allocations beyond the per-(class, attribute) cap.
    pub redirected_attributes: u64,
    pub budget_exhausted: bool,
}

// --- compact value sets -------------------------------------------------------------------

/// Sorted, deduplicated set of values (compact replacement of `BTreeSet<Value>`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Vals(Vec<Value>);

impl Vals {
    fn new() -> Vals {
        Vals(Vec::new())
    }

    fn one(v: Value) -> Vals {
        Vals(vec![v])
    }

    fn from_unsorted(mut v: Vec<Value>) -> Vals {
        v.sort_unstable();
        v.dedup();
        Vals(v)
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn iter(&self) -> std::slice::Iter<'_, Value> {
        self.0.iter()
    }

    fn as_slice(&self) -> &[Value] {
        &self.0
    }

    fn insert(&mut self, v: Value) -> bool {
        match self.0.binary_search(&v) {
            Ok(_) => false,
            Err(i) => {
                self.0.insert(i, v);
                true
            }
        }
    }

    fn remove(&mut self, v: &Value) -> bool {
        match self.0.binary_search(v) {
            Ok(i) => {
                self.0.remove(i);
                true
            }
            Err(_) => false,
        }
    }
}

impl Extend<Value> for Vals {
    fn extend<I: IntoIterator<Item = Value>>(&mut self, iter: I) {
        let before = self.0.len();
        self.0.extend(iter);
        if self.0.len() != before {
            self.0.sort_unstable();
            self.0.dedup();
        }
    }
}

impl<'a> Extend<&'a Value> for Vals {
    fn extend<I: IntoIterator<Item = &'a Value>>(&mut self, iter: I) {
        self.extend(iter.into_iter().copied());
    }
}

impl FromIterator<Value> for Vals {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Vals {
        Vals::from_unsorted(iter.into_iter().collect())
    }
}

impl IntoIterator for Vals {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Vals {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Solved flow state.
pub struct Flow<'a> {
    pub index: &'a Index,
    pub hierarchy: &'a Hierarchy,
    /// Rounds used by the solver (maximum over both phases; <= `flow.max_iterations`).
    pub iterations: usize,
    names: Interner,
    constraints: Vec<Constraint>,
    implicit: Vec<Implicit>,
    /// (owner, evidence file) -> sorted (evidence start byte, target) of proven call edges.
    calls: HashMap<(SymbolId, FileId), Vec<(u32, SymbolId)>>,
    /// Like `calls`, plus property getters (targets already executed at a location).
    executed: HashMap<(SymbolId, FileId), Vec<(u32, SymbolId)>>,
    /// Interned parameter names per symbol (index = symbol id).
    params: Vec<Vec<Name>>,
    selfs: HashMap<SymbolId, SelfParam>,
    /// Lambda / generator-expression symbols (closure scopes).
    anonymous: HashSet<SymbolId>,
    /// Names bound by assignment inside a callable (shadow enclosing parameters).
    locals: HashSet<(SymbolId, Name)>,
    /// Python bare-name reads the language's scoping proves local ([`scoping`]).
    local_reads: HashSet<(FileId, ByteSpan)>,
    /// (file, callee span) -> index of the syntax call site in that file's facts.
    call_sites: HashMap<(FileId, ByteSpan), u32>,
    /// Precomputed breadth-first base order per class.
    mros: HashMap<SymbolId, Vec<SymbolId>>,
    /// Class family (class and transitive subclasses) per receiver class.
    families: HashMap<SymbolId, Vec<SymbolId>>,
    /// Eval constraints per scope symbol (receiver specialisation).
    evals_by_scope: HashMap<SymbolId, Vec<usize>>,
    /// Synthetic `<module>` symbol per file (owner of module-level candidates).
    module_owner: Vec<Option<SymbolId>>,
    /// The solution (both views).
    state: State,
    /// Some constraint is test code (the product view differs from the full view).
    has_tests: bool,
    /// Parameters consumed (called / iterated) by their function.
    consumed: HashMap<(SymbolId, Name), Consume>,
    /// Attribute name -> classes that may define it as a member: a method of the class or
    /// a `Member` slot target of some constraint (class-body bindings, decorated members).
    /// Static for a solve, so MRO steps through other classes can be skipped unrecorded.
    definers: HashMap<Name, HashSet<SymbolId>>,
    /// Library symbols of library-created objects (`Value::Library` index), in first-use
    /// order over the files (deterministic: files are sorted by path).
    libraries: Vec<String>,
    /// The `flow` settings of this solve (bounds and budgets).
    settings: FlowSettings,
    /// Interned prototype names per language (index = `Language as usize`).
    prototypes: Vec<PrototypeNames>,
    /// Some delegation can exist in this solve (a prototype link fact or library knowledge
    /// with member copying / delegation): otherwise member lookups never consult delegates.
    delegation: bool,
    /// Library classes of repository types' bases ([`library_bases`]).
    library_bases: HashMap<SymbolId, Vec<trace_library::library_class::LibraryClass>>,
    /// Interned module-value variable of JavaScript files ([`crate::flow::js_objects`]).
    exports_name: Option<Name>,
}

/// Interned names of a language's prototype object model (`__proto__`, `prototype`: language
/// rules of `trace_library::languages::objects::ObjectModel`), when some fact names them.
#[derive(Clone, Copy, Debug, Default)]
struct PrototypeNames {
    /// The assignable prototype link of an object (`__proto__`).
    link: Option<Name>,
    /// The member of a class holding its instances' methods (`prototype`).
    prototype: Option<Name>,
}

/// Span of synthetic call nodes: matches no proven edge, candidate site or guard.
const NOWHERE: ByteSpan = ByteSpan::new(u32::MAX, u32::MAX);

mod calls;
mod candidates;
mod compile;
mod eval;
mod injections;
mod js_objects;
mod library_bases;
mod lookup;
mod raws;
mod receivers;
mod scoping;
mod solve;
mod state;

use calls::*;
use candidates::*;
use compile::*;
pub(crate) use compile::{test_files, test_symbols};
use eval::*;
use injections::*;
pub(crate) use injections::{index_injections, Injection};
pub(crate) use receivers::receiver_rule_candidates;
use receivers::*;
use state::*;

#[cfg(test)]
#[path = "../../tests/unit/flow/mod.rs"]
mod tests;
