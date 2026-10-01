//! Prototype object model of a language (JavaScript / TypeScript): objects whose members are
//! assigned functions are classes, `this` is the object a function is a member of, the
//! implicit `arguments` object, receiver-binding invocations (`f.call(t, ..)`,
//! `f.apply(t, arr)`, `f.bind(t)`), prototype links (`Object.setPrototypeOf(o, p)`,
//! `Object.create(p)`) and CommonJS module values (`module.exports = v`). Language rules
//! only; no library is named.

/// Language data of the prototype object model ([`super::AdapterSpec::objects`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectModel {
    /// Implicit receiver of a function called as a member (`this`).
    pub receiver: &'static str,
    /// Implicit object holding every argument of a (non-arrow) function call (`arguments`).
    pub arguments: &'static str,
    /// Member of a constructor function holding the methods of its instances (`prototype`).
    pub prototype: &'static str,
    /// Assignable prototype link of an object (`__proto__`).
    pub prototype_link: &'static str,
    /// Invocation with an explicit receiver and the arguments listed (`f.call(t, a, b)`).
    pub call_with_receiver: &'static str,
    /// Invocation with an explicit receiver and the arguments of an array (`f.apply(t, arr)`).
    pub apply_with_receiver: &'static str,
    /// A function bound to a receiver (`f.bind(t)`): the same function.
    pub bind_receiver: &'static str,
    /// Methods whose value holds the receiver's elements from an offset (`arr.slice(1)`).
    pub slice_methods: &'static [&'static str],
    /// Element methods whose value holds what the callback returns for every element
    /// (`names.map(n => n.toLowerCase())`).
    pub mapping_methods: &'static [&'static str],
    /// String methods returning the receiver in lower / upper case.
    pub lower_case_methods: &'static [&'static str],
    pub upper_case_methods: &'static [&'static str],
    /// Built-ins linking one argument's prototype to another's members:
    /// (callee spelling, object index, prototype index).
    pub prototype_setters: &'static [(&'static str, u32, u32)],
    /// Built-ins creating an object whose prototype is one argument: (callee spelling, index).
    pub prototype_creators: &'static [(&'static str, u32)],
    /// Module-level names of the module's exported value (`exports`) and the module object
    /// whose `exports` member is it (`module`).
    pub exports: &'static str,
    pub module: &'static str,
    /// Module loading call (`require("./x")`): its value is the loaded module.
    pub require: &'static str,
}

impl ObjectModel {
    /// Whether a module-level bind target receives the module's exported value
    /// (`module.exports = v`, `exports = v`). Shared by the derivation and the repository
    /// value flow.
    pub fn is_export_target(&self, target: &trace_core::facts::BindTarget) -> bool {
        use trace_core::facts::{BindTarget, Expr};
        match target {
            BindTarget::FieldOf {
                object: Expr::Name { name, .. },
                name: member,
            } => name == self.module && member == self.exports,
            BindTarget::Var { name, .. } => name == self.exports,
            _ => false,
        }
    }

    /// Whether `object.member` reads the module's exported value (`module.exports`).
    pub fn is_export_read(&self, object: &trace_core::facts::Expr, member: &str) -> bool {
        matches!(object, trace_core::facts::Expr::Name { name, .. } if name == self.module)
            && member == self.exports
    }

    /// The module a loading call `require("<target>")` names (its literal argument).
    pub(crate) fn required_module<'e>(
        &self,
        func: &trace_core::facts::Expr,
        args: &'e [trace_core::facts::Expr],
    ) -> Option<&'e str> {
        use trace_core::facts::Expr;
        match (func, args) {
            (Expr::Name { name, .. }, [Expr::Name { name: arg, .. }]) if name == self.require => {
                arg.strip_prefix(trace_syntax::lower::LITERAL_PREFIX)
            }
            _ => None,
        }
    }
}
