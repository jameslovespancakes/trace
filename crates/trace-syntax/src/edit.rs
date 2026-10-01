//! Removing declarations from extracted facts while keeping every local index consistent
//! (Python `@overload` stubs, TypeScript overload signatures).

use trace_core::facts::{BindTarget, Expr, FileFacts, FlowFact, Scope};

/// Remap the synthetic declarations referenced by lambda values inside `expr`.
fn remap_expr(expr: &mut Expr, exact: &dyn Fn(Option<u32>) -> Option<u32>) {
    match expr {
        Expr::Lambda { function, .. } => *function = exact(*function),
        Expr::Attr { object, .. } => remap_expr(object, exact),
        Expr::Call {
            func, args, kwargs, ..
        } => {
            remap_expr(func, exact);
            for a in args {
                remap_expr(a, exact);
            }
            for (_, v) in kwargs {
                remap_expr(v, exact);
            }
        }
        Expr::Choice(options) => {
            for o in options {
                remap_expr(o, exact);
            }
        }
        Expr::Await(inner) => remap_expr(inner, exact),
        Expr::Name { .. } | Expr::Opaque => {}
    }
}

/// Remove the flagged declarations (and their descendants) and remap every declaration
/// index in `facts`. Execution owners inside removed declarations become `None`; lexical
/// owners move to the nearest surviving ancestor; flow facts that refer to a removed
/// declaration are dropped.
pub(crate) fn remove_declarations(facts: &mut FileFacts, remove: &[bool]) {
    let n = facts.declarations.len();
    let mut removed: Vec<bool> = (0..n).map(|i| remove.get(i).copied().unwrap_or(false)).collect();
    if !removed.iter().any(|&r| r) {
        return;
    }
    // Pre-order: parents precede children.
    for i in 0..n {
        if let Some(p) = facts.declarations[i].parent {
            if removed.get(p as usize).copied().unwrap_or(false) {
                removed[i] = true;
            }
        }
    }
    let mut map: Vec<Option<u32>> = vec![None; n];
    let mut next = 0u32;
    for i in 0..n {
        if !removed[i] {
            map[i] = Some(next);
            next += 1;
        }
    }
    // Nearest surviving ancestor-or-self, per old index.
    let mut surviving: Vec<Option<u32>> = vec![None; n];
    for i in 0..n {
        let value = match map[i] {
            Some(m) => Some(m),
            None => facts.declarations[i]
                .parent
                .and_then(|p| surviving.get(p as usize).copied().flatten()),
        };
        surviving[i] = value;
    }
    let exact = |d: Option<u32>| d.and_then(|d| map.get(d as usize).copied().flatten());
    let lexical = |d: Option<u32>| d.and_then(|d| surviving.get(d as usize).copied().flatten());

    let old = std::mem::take(&mut facts.declarations);
    facts.declarations = old
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !removed[*i])
        .map(|(_, mut d)| {
            d.parent = exact(d.parent);
            d
        })
        .collect();
    for call in &mut facts.calls {
        call.owner = exact(call.owner);
        call.lexical_owner = lexical(call.lexical_owner);
    }
    for r in &mut facts.references {
        r.owner = exact(r.owner);
    }
    for c in &mut facts.callbacks {
        c.owner = exact(c.owner);
    }
    facts.module_decl = exact(facts.module_decl);
    for b in &mut facts.boundaries {
        b.owner = exact(b.owner);
        b.decl = exact(b.decl);
    }
    let scope = |s: Scope| match s {
        Scope::Module => Some(Scope::Module),
        Scope::Decl(d) => exact(Some(d)).map(Scope::Decl),
    };
    let remap = |mut e: Expr| {
        remap_expr(&mut e, &exact);
        e
    };
    let target = |t: BindTarget| -> Option<BindTarget> {
        Some(match t {
            BindTarget::Var { scope: s, name } => BindTarget::Var {
                scope: scope(s)?,
                name,
            },
            BindTarget::Member { class, name } => BindTarget::Member {
                class: exact(Some(class))?,
                name,
            },
            BindTarget::FieldOf { object, name } => BindTarget::FieldOf {
                object: remap(object),
                name,
            },
            other => other,
        })
    };
    let flow = std::mem::take(&mut facts.flow);
    facts.flow = flow
        .into_iter()
        .filter_map(|fact| {
            Some(match fact {
                FlowFact::Bind {
                    target: t,
                    value,
                    scope: s,
                } => FlowFact::Bind {
                    target: target(t)?,
                    value: remap(value),
                    scope: scope(s)?,
                },
                FlowFact::Return { function, value } => FlowFact::Return {
                    function: exact(Some(function))?,
                    value: remap(value),
                },
                FlowFact::Eval { scope: s, call } => FlowFact::Eval {
                    scope: scope(s)?,
                    call: remap(call),
                },
                FlowFact::Decorated {
                    scope: s,
                    target: t,
                    function,
                    decorators,
                } => FlowFact::Decorated {
                    scope: scope(s)?,
                    target: target(t)?,
                    function: exact(Some(function))?,
                    decorators: decorators.into_iter().map(&remap).collect(),
                },
                FlowFact::ImplicitSelf {
                    function,
                    param,
                    class,
                    is_class,
                } => FlowFact::ImplicitSelf {
                    function: exact(Some(function))?,
                    param,
                    class: exact(Some(class))?,
                    is_class,
                },
            })
        })
        .collect();
    let implicit = std::mem::take(&mut facts.implicit);
    facts.implicit = implicit
        .into_iter()
        .filter_map(|mut op| {
            op.scope = scope(op.scope)?;
            remap_expr(&mut op.subject, &exact);
            Some(op)
        })
        .collect();
    for detail in &mut facts.call_details {
        if let Some(receiver) = &mut detail.receiver {
            remap_expr(receiver, &exact);
        }
        for argument in &mut detail.arguments {
            remap_expr(&mut argument.value, &exact);
        }
        for guard in &mut detail.not_identical {
            remap_expr(guard, &exact);
        }
    }
    let anonymous = std::mem::take(&mut facts.anonymous);
    facts.anonymous = anonymous
        .into_iter()
        .filter_map(|mut a| {
            a.decl = exact(Some(a.decl))?;
            a.created_in = exact(a.created_in);
            Some(a)
        })
        .collect();
    let imports = std::mem::take(&mut facts.imports);
    facts.imports = imports
        .into_iter()
        .filter_map(|mut i| {
            i.scope = scope(i.scope)?;
            Some(i)
        })
        .collect();
}

#[cfg(test)]
#[path = "../tests/unit/edit.rs"]
mod tests;
