//! JavaScript object model rules of the repository value flow. The spellings and the
//! recognition of exports, loaders and prototype built-ins are the object model of the library
//! derivation (`trace_library::languages::objects::ObjectModel`), shared with it:
//!
//! * **Loading**: `require("./x")` whose relative specifier names exactly one repository
//!   JavaScript file (Node.js file resolution: the path itself, the path with a script
//!   extension, else the directory's `index` file; a file wins over a directory index) is the
//!   module value of that file (the call either binds a name, `var m = require(..)`, or is a
//!   whole-module re-export, `module.exports = require(..)`). Package names (no `./` / `../` prefix) are library modules:
//!   never resolved here.
//! * **Module value**: every value a file binds at module level to `module.exports` or to
//!   `exports` (`module.exports = createApp`, `var app = exports = module.exports = {}`);
//!   reading `module.exports` reads it. A file that never binds them exports its implicit
//!   `exports` object, whose members are the `exports.x = v` stores.
//! * **Prototype built-ins** (language rules, like the derivation's): a call spelled as one of
//!   the model's prototype creators (`Object.create(p)`) makes a new object whose member
//!   lookups continue in `p`; a prototype setter (`Object.setPrototypeOf(o, p)`) makes `o`'s
//!   lookups continue in `p` and returns `o`. They apply only when the call has no library
//!   knowledge linking members and the spelling's root is not a repository binding.
//!
//! The flow ([`crate::flow`]) keeps the module value in the module-scope variable named by
//! [`ObjectModel::exports`]; this module answers which file a loader names, which bind
//! targets / reads are the module value and which effects a built-in call has.

use std::cell::Cell;
use std::collections::HashSet;

use trace_core::facts::{BindTarget, Expr, FlowFact, ImportKind, Scope};
use trace_core::{ByteSpan, FileId, Index, Language};
use trace_library::languages::objects::ObjectModel;
use trace_library::{ArgSel, Effect};

use crate::narrow::ModuleMap;

/// Whether `language` has CommonJS module values (JavaScript).
fn commonjs(language: Language) -> bool {
    trace_syntax::language_rules::rules(language).commonjs_modules
}

/// Allocation byte of a file's implicit `exports` object (no source position holds it).
pub(crate) const IMPLICIT_EXPORTS_ALLOC: u32 = u32::MAX - 1;

/// CommonJS module facts of one index (JavaScript files only).
pub(crate) struct JsObjects {
    model: ObjectModel,
    map: ModuleMap,
    /// JavaScript files that bind their module value themselves (no implicit object).
    whole: HashSet<FileId>,
    /// Some built-in call got prototype effects (member lookups consult delegates).
    linked: Cell<bool>,
}

impl JsObjects {
    /// `None` when the index has no file of a language with CommonJS modules
    /// (`LanguageRules::commonjs_modules`) or the language has no object model.
    pub(crate) fn new(index: &Index) -> Option<JsObjects> {
        let language = index.files.iter().map(|f| f.language).find(|&l| commonjs(l))?;
        let model = trace_library::languages::adapter(language)?.objects.clone()?;
        let whole = index
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| commonjs(f.language))
            .filter(|(_, f)| {
                f.facts.as_ref().is_some_and(|facts| {
                    facts.flow.iter().any(|fact| {
                        matches!(fact, FlowFact::Bind { target, scope: Scope::Module, .. }
                            if model.is_export_target(target))
                    })
                })
            })
            .map(|(fi, _)| FileId(fi as u32))
            .collect();
        Some(JsObjects {
            model,
            map: ModuleMap::new(index),
            whole,
            linked: Cell::new(false),
        })
    }

    /// Whether the rules apply to `file` (a file of a language with CommonJS modules).
    pub(crate) fn applies(index: &Index, file: FileId) -> bool {
        commonjs(index.file(file).language)
    }

    /// Module-scope variable holding a file's module value.
    pub(crate) fn exports_name(&self) -> &'static str {
        self.model.exports
    }

    /// Whether `file` exports its implicit `exports` object (it binds no module value).
    pub(crate) fn implicit_exports(&self, file: FileId) -> bool {
        !self.whole.contains(&file)
    }

    /// Whether a module-level bind target receives the module value.
    pub(crate) fn is_export_target(&self, target: &BindTarget) -> bool {
        self.model.is_export_target(target)
    }

    /// Whether `object.attr` reads the module value (`module.exports`).
    pub(crate) fn is_export_read(&self, object: &Expr, attr: &str) -> bool {
        self.model.is_export_read(object, attr)
    }

    /// The repository file a loader call `require("./x")` (callee `func`, whole call `span`)
    /// in `from` loads (module docs). The specifier is the one of the import binding the
    /// call makes (`var m = require("./x")`: `FileFacts::imports`, kind `Module`) or of the
    /// whole-module re-export at the call (`module.exports = require("./x")`:
    /// `FileFacts::exports` `*`); flow facts leave literal text out.
    pub(crate) fn loaded_file(
        &self,
        index: &Index,
        from: FileId,
        func: &Expr,
        span: ByteSpan,
    ) -> Option<FileId> {
        if !matches!(func, Expr::Name { name, .. } if name == self.model.require) {
            return None;
        }
        let facts = index.file(from).facts.as_ref()?;
        let reexports = facts
            .exports
            .iter()
            .filter(|e| e.exported == "*" && e.span == span)
            .map(|e| e.target.as_str());
        let mut bindings = facts
            .imports
            .iter()
            .filter(|i| i.kind == ImportKind::Module && i.span.start <= span.start && span.end <= i.span.end)
            .map(|i| i.target.as_str())
            .chain(reexports);
        let specifier = bindings.next()?;
        if bindings.next().is_some() {
            return None;
        }
        let relative =
            matches!(specifier, "." | "..") || specifier.starts_with("./") || specifier.starts_with("../");
        if !relative {
            return None;
        }
        let files = self.map.script_files(index, index.file_path(from), specifier);
        let js: Vec<FileId> = files
            .into_iter()
            .filter(|&f| commonjs(index.file(f).language))
            .collect();
        match js.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Language-rule effects of a prototype built-in call (module docs): `Object.create(p)`
    /// delegates the result to `p`; `Object.setPrototypeOf(o, p)` delegates `o` to `p` and
    /// returns `o`. `bound(span)`: the spelling's root name at `span` is a repository binding.
    pub(crate) fn builtin_effects(&self, func: &Expr, bound: impl Fn(ByteSpan) -> bool) -> Vec<Effect> {
        let Some((spelling, root)) = spelling(func) else {
            return Vec::new();
        };
        if bound(root) {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Some(&(_, i)) = self.model.prototype_creators.iter().find(|(n, _)| *n == spelling) {
            out.push(Effect::DelegatesMembers {
                object: ArgSel::Result,
                to: ArgSel::Pos(i),
            });
        }
        if let Some(&(_, o, p)) = self.model.prototype_setters.iter().find(|(n, _, _)| *n == spelling) {
            out.push(Effect::DelegatesMembers {
                object: ArgSel::Pos(o),
                to: ArgSel::Pos(p),
            });
            out.push(Effect::Returns(ArgSel::Pos(o)));
        }
        if !out.is_empty() {
            self.linked.set(true);
        }
        out
    }

    /// Whether some built-in call got prototype effects ([`JsObjects::builtin_effects`]).
    pub(crate) fn linked(&self) -> bool {
        self.linked.get()
    }
}

/// Dotted spelling of a callee (`Object.create`) and the span of its root name.
fn spelling(e: &Expr) -> Option<(String, ByteSpan)> {
    match e {
        Expr::Name { name, span } => Some((name.clone(), *span)),
        Expr::Attr { object, attr, .. } => spelling(object).map(|(o, root)| (format!("{o}.{attr}"), root)),
        _ => None,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/flow/js_objects.rs"]
mod tests;
