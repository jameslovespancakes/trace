//! Which dispatched stores are registrations (child of `derive`).
//!
//! Rule "a table owner's plain attributes are configuration": a class whose instances own a
//! dispatched container of entry objects (`self.routes` holding route objects the entry
//! matches) is a table owner; a callable stored into one of its plain attributes without a
//! key (`self.lifespan = lifespan`, `self.default = default`) is the table's own
//! configuration, run for every request, not an entry registered under a key. Keyed stores
//! (`self.handlers[name] = handler`) and the attributes of entry objects (a route
//! object's `self.endpoint`) stay registrations.
//!
//! Rule "mount targets are registries": a parameter whose declared type is a library class
//! that neither owns a table of entry objects nor is callable (`model: Any`, a model or record
//! type) is never a mounted sub-registry.

use super::{Program, SlotOwner, State, ITERATES};

impl<'c> Program<'c> {
    /// Classes owning a container slot of instances whose methods entry-reachable code calls
    /// (after dispatch).
    pub(super) fn compute_table_owners(&self, st: &mut State) {
        let mut owners = Vec::new();
        for (&s, insts) in &st.slot_insts {
            if insts.is_empty() || !st.method_dispatched.contains_key(&s) {
                continue;
            }
            if let Some(SlotOwner::Class(c)) = st.slot_keys.get(s as usize).map(|k| k.owner) {
                owners.push(c);
            }
        }
        st.table_owners.extend(owners);
    }

    /// Whether slot `s` is a plain (not container) attribute of a table owner (or of a
    /// related class).
    pub(super) fn table_configuration(&self, st: &State, s: u32) -> bool {
        let Some(SlotOwner::Class(c)) = st.slot_keys.get(s as usize).map(|k| k.owner) else {
            return false;
        };
        // Containers (iterated, or their elements' methods called) hold entries.
        if st.slot_mask[s as usize] & ITERATES != 0 || st.method_dispatched.contains_key(&s) {
            return false;
        }
        std::iter::once(c)
            .chain(self.related.get(c as usize).into_iter().flatten().copied())
            .any(|x| st.table_owners.contains(&x))
    }

    /// Whether parameter `t` of `f` can be a mounted registry: untyped, typed with a type the
    /// loaded source does not declare, or typed as a table owner / callable class.
    pub(super) fn mount_target(&self, st: &State, f: u32, t: u16) -> bool {
        let Some(ty) = self.param_types.get(&(f, t)) else {
            return true;
        };
        let Some(c) = self.declared_class(self.funcs[f as usize].unit, ty) else {
            return true;
        };
        std::iter::once(c)
            .chain(self.related.get(c as usize).into_iter().flatten().copied())
            .any(|x| {
                st.table_owners.contains(&x)
                    || self
                        .spec
                        .call_method
                        .is_some_and(|m| self.classes[x as usize].methods.contains_key(m))
            })
    }
}
