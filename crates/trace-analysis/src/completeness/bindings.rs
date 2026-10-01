//! Member-binding rule: whether a member access or bare name can denote a family member in
//! its language ([`Bindings`]).

use trace_core::model::{ByteSpan, FileId, SymbolId};
use trace_core::{Index, SymbolKind};

use super::{access::is_free_function, access::is_member, access::package_qualifier, Access};
use crate::cards::executing_symbol_at;
use crate::languages::{rules, BareMember};

/// The member-binding rule for one family (SPEC §10.1 `member_binding`).
pub struct Bindings<'i> {
    pub(super) index: &'i Index,
    pub(super) family: Vec<SymbolId>,
}

impl<'i> Bindings<'i> {
    pub fn new(index: &'i Index, family: &[SymbolId]) -> Bindings<'i> {
        Bindings {
            index,
            family: family.to_vec(),
        }
    }

    /// Whether an occurrence at (`file`, `span`) with `access` can never denote family
    /// member `m`:
    /// * a member access of a free function (languages whose free functions are never
    ///   members, `AnalysisRules::free_functions_not_members`) unless the
    ///   receiver root is bound by an import in that file (module / namespace object:
    ///   `mod.f()`, `import * as ns`, `from pkg import mod`) or the target is declared on the
    ///   receiver as a module table (`M.f`); receivers that are neither the self reference nor
    ///   a plain name (`require("m").f()`) are never judged;
    /// * a bare name of a member (languages without an implicit receiver,
    ///   `AnalysisRules::no_implicit_receiver`, SPEC §10.1) unless [`Bindings::bare_exempt`].
    pub fn excludes(&self, file: FileId, span: ByteSpan, access: &Access, m: SymbolId) -> bool {
        let index = self.index;
        let s = index.symbol(m);
        match access {
            Access::Unknown => false,
            Access::Member {
                receiver_root,
                self_receiver,
            } => {
                if !rules(s.language).free_functions_not_members || !is_free_function(index, m) {
                    return false;
                }
                match receiver_root {
                    Some(root) => {
                        if s.qualified_name == format!("{root}.{}", s.name) {
                            return false;
                        }
                        let imported = index
                            .file(file)
                            .facts
                            .as_ref()
                            .is_some_and(|f| f.imports.iter().any(|i| &i.local == root));
                        !imported && !package_qualifier(index, m, root)
                    }
                    None => *self_receiver,
                }
            }
            Access::Bare => {
                rules(index.file(file).language).no_implicit_receiver
                    && is_member(index, m)
                    && !self.bare_exempt(file, span, m)
            }
        }
    }

    /// Where a bare name can denote member `m` although the language has no implicit
    /// receiver (`AnalysisRules::bare_member`): inside the member's own declaration (a named
    /// function expression's own name, `res.redirect = function redirect() { ... redirect
    /// ... }`), code at class-body level of the declaring class (not inside a method), or
    /// inside the declaring type / module body.
    fn bare_exempt(&self, file: FileId, span: ByteSpan, m: SymbolId) -> bool {
        let index = self.index;
        let s = index.symbol(m);
        if s.file != file {
            return false;
        }
        let parent = s.parent.map(|p| index.symbol(p)).filter(|p| p.kind.is_type());
        match rules(index.file(file).language).bare_member {
            BareMember::OwnDeclaration => s.span.bytes.contains(span.start),
            BareMember::ClassBody => parent.is_some_and(|p| {
                p.span.bytes.contains(span.start)
                    && executing_symbol_at(index, file, span.start).is_none_or(|e| {
                        let e = index.symbol(e);
                        e.kind == SymbolKind::Module || e.span.bytes.encloses(p.span.bytes)
                    })
            }),
            BareMember::TypeBody => parent.is_some_and(|p| p.span.bytes.contains(span.start)),
        }
    }

    /// [`Bindings::excludes`] for an occurrence without a resolved member: every family
    /// member named `name` is excluded (false when no member has that name, e.g. a class
    /// name standing for its constructor).
    pub(crate) fn excludes_name(&self, file: FileId, span: ByteSpan, access: &Access, name: &str) -> bool {
        let mut named = self
            .family
            .iter()
            .copied()
            .filter(|&m| {
                let s = self.index.symbol(m);
                !s.is_synthetic() && s.name == name
            })
            .peekable();
        named.peek().is_some() && named.all(|m| self.excludes(file, span, access, m))
    }
}

// ------------------------------------------------------------------ family candidates
