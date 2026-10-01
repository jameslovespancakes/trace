//! Solver state of the value flow: slots, cells, constraints, receiver contexts and the
//! interned names (child of [`crate::flow`]).

use super::*;

/// Per-class part of an instance attribute lookup (shared by the allocations of a class).
pub(super) struct ClassLookup<'s> {
    pub(super) member: Option<Result<SlotRef<'s>, SymbolId>>,
    pub(super) any: Vals,
    pub(super) class_slot: Option<Option<SlotRef<'s>>>,
}

/// A slot's values as seen by one view (product values, then test-origin values).
#[derive(Clone, Copy, Debug)]
pub(super) struct SlotRef<'s> {
    pub(super) prod: &'s [Value],
    pub(super) test: &'s [Value],
}

impl<'s> SlotRef<'s> {
    pub(super) fn iter(self) -> impl Iterator<Item = &'s Value> {
        self.prod.iter().chain(self.test.iter())
    }

    pub(super) fn is_empty(self) -> bool {
        self.prod.is_empty() && self.test.is_empty()
    }

    pub(super) fn to_vals(self) -> Vals {
        if self.test.is_empty() {
            Vals(self.prod.to_vec())
        } else {
            Vals::from_unsorted(self.iter().copied().collect())
        }
    }
}

// --- internal IR ----------------------------------------------------------------------

/// Interned identifier.
pub(super) type Name = u32;
/// A variable of a symbol scope (parameter or local).
pub(super) type ParamKey = (SymbolId, Name);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ScopeKey {
    Module(FileId),
    Symbol(SymbolId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Slot {
    Var(ScopeKey, Recv, Name),
    /// Union of `Var(scope, *, name)` over contexts (closure reads).
    VarAll(ScopeKey, Name),
    Default(SymbolId, Name),
    Member(SymbolId, Name),
    Field(Name),
    ObjAttr(Obj, Name),
    ClassAttr(SymbolId, Name),
    Return(SymbolId, Recv),
    /// Property `name` of a plain object, function object or library object.
    Prop(Holder, Name),
    /// Values a member lookup on the holder continues in when the holder has no such member
    /// (copied mixins, prototypes, metatable `__index` tables).
    Delegates(Holder),
}

/// A state cell an evaluation can read: a slot, the receiver contexts of a method, or the
/// allocations with an attribute slot of (class, attribute).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Dep {
    Slot(Slot),
    Contexts(SymbolId),
    AttrObjs(SymbolId, Name),
    /// Any attribute cell of this name (`Member`, `ClassAttr`, `ObjAttr` slots and
    /// `AttrObjs` lists of every class). Attribute lookups walk whole MROs and receiver
    /// families; recording one coarse dependency per name instead of one per (class, name)
    /// keeps the per-item dependency lists small on large class hierarchies. Coarser
    /// dependencies only cause extra re-evaluations, never a different fixpoint.
    Attr(Name),
    /// Change clock of one kind of attribute cell of this name (0 `Member`, 1 `ClassAttr`,
    /// 2 `ObjAttr`); validity of the MRO lookup memo only, never recorded as a dependency.
    AttrClock(u8, Name),
}

/// The coarse dependency an attribute read is recorded under ([`Dep::Attr`]).
pub(super) fn recorded_dep(dep: Dep) -> Dep {
    match dep {
        Dep::Slot(Slot::Member(_, n) | Slot::ClassAttr(_, n) | Slot::ObjAttr(_, n)) | Dep::AttrObjs(_, n) => {
            Dep::Attr(n)
        }
        other => other,
    }
}

#[derive(Debug)]
pub(super) enum Node {
    Name {
        name: Name,
        target: Option<SymbolId>,
        span: ByteSpan,
    },
    Attr {
        object: Box<Node>,
        attr: Name,
        target: Option<SymbolId>,
        /// Attribute identifier span.
        span: ByteSpan,
    },
    Call(Box<CallNode>),
    Choice(Vec<Node>),
    /// Anonymous scope value (lambda / generator expression symbol).
    Lambda(SymbolId),
    /// Object / table literal: a new plain object allocated at `alloc` (start byte) with
    /// keyed members.
    Object {
        alloc: u32,
        span: ByteSpan,
        fields: Vec<(Name, Node)>,
    },
    /// The values generator `function` yields (every context).
    Yielded {
        function: SymbolId,
        name: Name,
    },
    /// A string / number / boolean / null literal: no object.
    Literal,
    /// The module value of a JavaScript file (`require("./x")`, `module.exports`;
    /// [`crate::flow::js_objects`]).
    Module(FileId),
    Opaque,
}

#[derive(Debug)]
pub(super) struct CallNode {
    pub(super) func: Node,
    pub(super) func_span: ByteSpan,
    pub(super) span: ByteSpan,
    pub(super) args: Vec<Node>,
    pub(super) kwargs: Vec<(Name, Node)>,
    /// Library effects of this call on its arguments (knowledge of the call at its callee
    /// start; Python `super()`: `Receiver`); they apply only when the callee has no in-index
    /// value.
    pub(super) effects: Vec<Effect>,
    /// Identity guards: objects the (bare-name) callee is proven not to be at this call.
    pub(super) not_identical: Vec<Node>,
    /// The server resolved the callee into library code (or the call has library knowledge
    /// naming its symbol): index into `Flow::libraries`. Module loaders (calls inside an
    /// import binding, `const m = require("m")`) have none: they return a module, not an
    /// object the library made.
    pub(super) library: Option<u32>,
}

impl CallNode {
    /// (positional index, keyword, argument).
    pub(super) fn arguments(&self) -> impl Iterator<Item = (Option<usize>, Option<Name>, &Node)> {
        self.args
            .iter()
            .enumerate()
            .map(|(i, a)| (Some(i), None, a))
            .chain(self.kwargs.iter().map(|(k, a)| (None, Some(*k), a)))
    }

    pub(super) fn is_super(&self) -> bool {
        self.effects.contains(&Effect::Receiver)
    }
}

#[derive(Debug)]
pub(super) enum Target {
    Var(ScopeKey, Name),
    Member(SymbolId, Name),
    Field(Name),
    FieldOf(Node, Name),
}

#[derive(Debug)]
pub(super) enum Rule {
    Bind {
        target: Target,
        value: Node,
    },
    Return {
        function: SymbolId,
        value: Node,
    },
    Eval {
        call: CallNode,
    },
    Decorated {
        target: Target,
        function: SymbolId,
        /// Decorator expressions (outermost first) with their allocation byte and the
        /// library effects of applying them to the decorated function (argument 0).
        decorators: Vec<(Node, u32, Vec<Effect>)>,
    },
    /// Implicit data-model operation (index into `Flow::implicit`).
    Implicit(usize),
}

#[derive(Debug)]
pub(super) struct Constraint {
    pub(super) rule: Rule,
    pub(super) scope: ScopeKey,
    pub(super) file: FileId,
    /// Evaluated in test code.
    pub(super) test: bool,
}

#[derive(Debug)]
pub(super) struct Implicit {
    pub(super) owner: SymbolId,
    pub(super) file: FileId,
    pub(super) kind: ImplicitKind,
    pub(super) subject: Node,
    pub(super) span: ByteSpan,
    pub(super) line: u32,
}

/// Receiver parameter of a method (`ImplicitSelf`).
#[derive(Clone, Copy, Debug)]
pub(super) struct SelfParam {
    pub(super) name: Name,
    pub(super) class: SymbolId,
    pub(super) is_class: bool,
}

impl SelfParam {
    pub(super) fn declared(self) -> Recv {
        if self.is_class {
            Recv::Class(self.class)
        } else {
            Recv::Instance(Obj::any(self.class))
        }
    }
}

#[derive(Default)]
pub(super) struct Interner {
    pub(super) ids: HashMap<String, Name>,
    pub(super) names: Vec<String>,
}

impl Interner {
    pub(super) fn intern(&mut self, s: &str) -> Name {
        if let Some(&id) = self.ids.get(s) {
            return id;
        }
        let id = self.names.len() as Name;
        self.names.push(s.to_string());
        self.ids.insert(s.to_string(), id);
        id
    }
    pub(super) fn get(&self, s: &str) -> Option<Name> {
        self.ids.get(s).copied()
    }
    pub(super) fn text(&self, id: Name) -> &str {
        &self.names[id as usize]
    }
}

/// Pending effects of one evaluation: slot writes and receiver contexts.
#[derive(Default)]
pub(super) struct Eff {
    pub(super) strict: bool,
    pub(super) writes: Vec<(Slot, Vals)>,
    pub(super) contexts: Vec<(SymbolId, Recv)>,
}

impl Eff {
    pub(super) fn strict() -> Eff {
        Eff {
            strict: true,
            ..Eff::default()
        }
    }

    pub(super) fn write(&mut self, slot: Slot, vals: Vals) {
        if !vals.is_empty() {
            self.writes.push((slot, vals));
        }
    }
}

/// Which values an evaluation sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum View {
    /// Values derived without test code only (the product-only fixpoint).
    Product,
    /// Every value.
    Full,
}

/// One state cell: a slot's values split by origin (product / test), and bookkeeping.
#[derive(Default)]
pub(super) struct Cell {
    /// Sorted values derived without test code.
    pub(super) prod: Vec<Value>,
    /// Sorted values first derived with test code (disjoint from `prod`).
    pub(super) test: Vec<Value>,
    /// Clock of the last change (0 = never).
    pub(super) changed_at: u32,
    pub(super) widened: bool,
    pub(super) saturated: bool,
}

impl Cell {
    pub(super) fn len(&self) -> usize {
        self.prod.len() + self.test.len()
    }

    pub(super) fn widen(&mut self) {
        self.widened = true;
        for v in self.prod.iter_mut() {
            *v = v.widened();
        }
        self.prod.sort_unstable();
        self.prod.dedup();
        for v in self.test.iter_mut() {
            *v = v.widened();
        }
        self.test.sort_unstable();
        self.test.dedup();
        let prod = &self.prod;
        self.test.retain(|v| prod.binary_search(v).is_err());
    }

    fn truncate(&mut self, max: usize) {
        if self.prod.len() >= max {
            self.prod.truncate(max);
            self.test.clear();
        } else {
            let room = max - self.prod.len();
            self.test.truncate(room);
        }
    }
}

/// Reads recorded during one evaluation.
#[derive(Default)]
pub(super) struct Rec {
    pub(super) ids: Vec<u32>,
    pub(super) missing: Vec<Dep>,
    /// A saturated slot was read.
    pub(super) saturated: bool,
}

/// Solver bookkeeping of one (constraint, context) item.
pub(super) struct Item {
    pub(super) ctx: Recv,
    /// Clock of the last evaluation.
    pub(super) at: u32,
    /// Cells read by the last evaluation (sorted).
    pub(super) deps: Box<[u32]>,
}

/// The solution of the constraint system (both views, see module docs).
#[derive(Default)]
pub(super) struct State {
    pub(super) ids: HashMap<Dep, u32>,
    pub(super) cells: Vec<Cell>,
    /// method -> receiver contexts sorted by `Recv`, with their test-origin bit.
    pub(super) contexts: HashMap<SymbolId, Vec<(Recv, bool)>>,
    /// (class, attribute) -> allocations with an `ObjAttr` slot, sorted, with test bit.
    pub(super) attr_objs: HashMap<(SymbolId, Name), Vec<(Obj, bool)>>,
    pub(super) clock: u32,
    /// Writes carry the test-origin bit (second phase).
    pub(super) testing: bool,
    pub(super) stats: FlowStats,
    pub(super) bound_slots: Vec<Slot>,
    /// Classes with a written `ClassAttr(k, name)` cell (kind 1) or `ObjAttr(Any(k), name)`
    /// cell (kind 2), by name: MRO walks only look these up (a new one touches
    /// [`Dep::Attr`], which every such walk records).
    pub(super) attr_classes: HashMap<(u8, Name), HashSet<SymbolId>>,
    /// The `flow` settings bounding slots and examples.
    pub(super) limits: FlowSettings,
}

impl State {
    pub(super) fn intern(&mut self, dep: Dep) -> u32 {
        if let Some(&id) = self.ids.get(&dep) {
            return id;
        }
        let id = self.cells.len() as u32;
        self.cells.push(Cell::default());
        self.ids.insert(dep, id);
        id
    }

    fn touch(&mut self, dep: Dep, t: u32) {
        let id = self.intern(dep) as usize;
        self.cells[id].changed_at = t;
    }

    pub(super) fn deps_of(&mut self, rec: Rec) -> Box<[u32]> {
        let Rec { mut ids, missing, .. } = rec;
        for d in missing {
            ids.push(self.intern(d));
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_boxed_slice()
    }

    pub(super) fn is_dirty(&self, item: &Item) -> bool {
        item.deps
            .iter()
            .any(|&d| self.cells[d as usize].changed_at >= item.at)
    }

    pub(super) fn seed_context(&mut self, f: SymbolId, r: Recv) {
        let list = self.contexts.entry(f).or_default();
        if let Err(i) = list.binary_search_by(|(x, _)| x.cmp(&r)) {
            list.insert(i, (r, false));
        }
    }

    pub(super) fn apply(&mut self, eff: Eff, t: u32) -> bool {
        let mut changed = false;
        for (slot, vals) in eff.writes {
            changed |= self.add(slot, vals, t);
        }
        for (f, r) in eff.contexts {
            changed |= self.add_context(f, r, t);
        }
        changed
    }

    fn add_context(&mut self, f: SymbolId, r: Recv, t: u32) -> bool {
        let testing = self.testing;
        let inserted = {
            let list = self.contexts.entry(f).or_default();
            match list.binary_search_by(|(x, _)| x.cmp(&r)) {
                Ok(_) => false,
                Err(i) => {
                    list.insert(i, (r, testing));
                    true
                }
            }
        };
        if inserted {
            self.touch(Dep::Contexts(f), t);
        }
        inserted
    }

    pub(super) fn add(&mut self, slot: Slot, vals: Vals, t: u32) -> bool {
        if vals.is_empty() {
            return false;
        }
        let slot = match slot {
            Slot::ObjAttr(obj, name) => self.register_obj(obj, name, t),
            other => other,
        };
        if let Slot::Var(scope, _, name) = slot {
            self.add_values(Slot::VarAll(scope, name), &vals, t);
        }
        match slot {
            Slot::ClassAttr(k, n) => {
                self.attr_classes.entry((1, n)).or_default().insert(k);
            }
            Slot::ObjAttr(o, n) if o.alloc == Alloc::Any => {
                self.attr_classes.entry((2, n)).or_default().insert(o.class);
            }
            _ => {}
        }
        self.add_values(slot, &vals, t)
    }

    /// Record an allocation with an attribute slot; beyond the cap the write goes to the
    /// class's `Any` object (read by every allocation of the class).
    fn register_obj(&mut self, obj: Obj, name: Name, t: u32) -> Slot {
        let testing = self.testing;
        let outcome = {
            let list = self.attr_objs.entry((obj.class, name)).or_default();
            match list.binary_search_by(|(o, _)| o.cmp(&obj)) {
                Ok(_) => Some(false),
                Err(i) if list.len() < self.limits.max_slot_values || obj.alloc == Alloc::Any => {
                    list.insert(i, (obj, testing));
                    Some(true)
                }
                Err(_) => None,
            }
        };
        match outcome {
            Some(true) => {
                self.touch(Dep::AttrObjs(obj.class, name), t);
                self.touch(Dep::Attr(name), t);
                Slot::ObjAttr(obj, name)
            }
            Some(false) => Slot::ObjAttr(obj, name),
            None => {
                self.stats.redirected_attributes += 1;
                Slot::ObjAttr(Obj::any(obj.class), name)
            }
        }
    }

    fn add_values(&mut self, slot: Slot, vals: &Vals, t: u32) -> bool {
        let id = self.intern(Dep::Slot(slot)) as usize;
        let testing = self.testing;
        let cell = &mut self.cells[id];
        if cell.saturated {
            return false;
        }
        let mut added = false;
        for &v in vals.iter() {
            if cell.prod.binary_search(&v).is_ok() {
                continue;
            }
            let side = if testing { &mut cell.test } else { &mut cell.prod };
            if let Err(i) = side.binary_search(&v) {
                side.insert(i, v);
                added = true;
            }
        }
        if !added {
            return false;
        }
        cell.changed_at = t;
        let max = self.limits.max_slot_values;
        if cell.len() > max {
            if !cell.widened {
                self.stats.widened_slots += 1;
            }
            cell.widen();
            if cell.len() > max {
                cell.truncate(max);
                cell.saturated = true;
                self.stats.saturated_slots += 1;
                if self.bound_slots.len() < self.limits.bound_examples {
                    self.bound_slots.push(slot);
                }
            }
        }
        if let Dep::Attr(n) = recorded_dep(Dep::Slot(slot)) {
            self.touch(Dep::Attr(n), t);
            let kind = match slot {
                Slot::Member(..) => 0,
                Slot::ClassAttr(..) => 1,
                _ => 2,
            };
            self.touch(Dep::AttrClock(kind, n), t);
        }
        true
    }
}

/// Consumption of a parameter by its function.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Consume {
    pub(super) call: bool,
    pub(super) iterate: bool,
    pub(super) advance: bool,
}

impl Consume {
    pub(super) fn any(self) -> bool {
        self.call || self.iterate || self.advance
    }

    pub(super) fn merge(&mut self, other: Consume) -> bool {
        let before = *self;
        self.call |= other.call;
        self.iterate |= other.iterate;
        self.advance |= other.advance;
        *self != before
    }
}

/// How a library call consumes its argument at `pos` / `kw` (of `positional` positional
/// arguments): called (now or later: `calls`, `stored_then_called`, a registered handler),
/// iterated or advanced. `never_calls` on the argument cancels a call.
pub(super) fn library_consume(
    effects: &[Effect],
    pos: Option<usize>,
    kw: Option<&str>,
    positional: usize,
) -> Consume {
    let mut c = Consume::default();
    let mut never = false;
    for e in effects {
        let Some(sel) = crate::behaviour::selector(e) else { continue };
        if !selects(sel, pos, kw, positional) {
            continue;
        }
        match e {
            Effect::Calls(_) | Effect::StoredThenCalled(_) | Effect::Registers { .. } => c.call = true,
            Effect::Iterates(_) => c.iterate = true,
            Effect::Advances(_) => c.advance = true,
            Effect::NeverCalls(_) => never = true,
            _ => {}
        }
    }
    if never {
        c.call = false;
    }
    c
}

/// Evaluation scope: where, under which receiver context, in which file.
#[derive(Clone, Copy, Debug)]
pub(super) struct Sc {
    pub(super) scope: ScopeKey,
    pub(super) ctx: Recv,
    pub(super) file: FileId,
}
