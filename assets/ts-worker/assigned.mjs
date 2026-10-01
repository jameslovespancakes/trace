// Assigned function values inside the TypeScript checker (owner script; checker facts and the
// syntax tree, no tables).
//
// Rule "assigned function": a call whose signature comes from a type (an interface / alias
// call signature, a function type) reaches the repository function its callee HOLDS when
// the callee is a variable, a class property or an object-literal property whose initializer
// and every write in the repository assign one and the same repository function:
// * writes: `x = f`, `this.p = f`, `obj.p = f`, `this[k] = f` where the checker types the key
//   `k` as a string literal or a union of string literals naming `p` (one write for every
//   named property: `this[method] = (...) => ...` over `method: 'get' | 'post' | ...`),
//   `x ??= f` / `x ||= f` / `x &&= f`;
// * values: a function expression / arrow, a repository function / method or another held
//   value named by an identifier or a property access (followed), either branch of
//   `c ? f : g` / `f ?? g` / `f || g`, the function a call returns (rule "returned
//   function" below), seen through parentheses, `as`, `satisfies`, `!` and `<T>`
//   assertions; `null`, `undefined`, literals, object and array literals add no function
//   (calling them throws: they never run repository code);
// * any other value makes the holder unknown and the rule gives nothing: a parameter, a
//   destructuring target, a compound assignment, a `for-in` / `for-of` / `catch` binding;
// * a write the checker cannot attribute (the object is `any` / an error type) poisons every
//   property of that name; a write with a non-literal key on an object typed by a repository
//   class or object literal (each member of a union) poisons every property of that class /
//   literal;
// * a class property whose name a member of another, derived (`extends`) class declares gives
//   nothing (the subclass member may shadow it on the receiver);
// * writes are collected syntactically over EVERY repository file of every project (each file
//   once) and attributed lazily, per written name, with the checker of the file's own
//   project; holders are keyed by their declaration (file and position), so a write in a
//   test file counts for a call in a source file.
// Rule "returned function": a call returns the one repository function its callee - resolved
// by the checker to a repository function with a body, neither async nor a generator -
// returns from its expression body or from every `return` of its own body (nested functions
// excluded); a call of that call reaches it (`compose(stack)(context)`), a holder assigned the
// call holds it (`const handler = handle(app)`).
// Residuals (documented, not modelled): writes through `Object.assign`,
// `Object.defineProperty`, `Reflect.set`, writes by code outside the repository, and
// `anyValue[dynamicKey] = f`.

const MAX_DEPTH = 6;

/**
 * Index the holders and writes of every group's sources; returns
 * `{functionOf(symbol, project)}`: the repository function id the symbol's declarations hold,
 * or null. `env = {K, SymbolFlags, declarations, key, fileOf, diagnostics}`.
 */
export function assignedFunctions(groups, env) {
  const {K, SymbolFlags, declarations, key, fileOf, diagnostics} = env;
  const assignValue = new Set([K.EqualsToken, K.BarBarEqualsToken, K.AmpersandAmpersandEqualsToken, K.QuestionQuestionEqualsToken]);
  const literalKinds = new Set([K.NullKeyword, K.TrueKeyword, K.FalseKeyword, K.NumericLiteral, K.BigIntLiteral, K.StringLiteral,
    K.NoSubstitutionTemplateLiteral, K.TemplateExpression, K.RegularExpressionLiteral, K.ObjectLiteralExpression,
    K.ArrayLiteralExpression, K.VoidExpression]);
  const wrappers = new Set([K.ParenthesizedExpression, K.AsExpression, K.SatisfiesExpression, K.NonNullExpression, K.TypeAssertionExpression]);
  const nested = new Set([K.FunctionDeclaration, K.FunctionExpression, K.ArrowFunction, K.MethodDeclaration, K.Constructor,
    K.GetAccessor, K.SetAccessor, K.ClassDeclaration, K.ClassExpression]);
  const containerKinds = new Set([K.ClassDeclaration, K.ClassExpression, K.ObjectLiteralExpression]);
  const unwrap = e => { while (e && wrappers.has(e.kind)) e = e.expression; return e; };
  // Holder declaration key -> {name, owner (class key), container (class / object literal
  // key), values}; a value is {fn} | {none} |
  // {unknown} | {alias: node, group} (resolved on demand).
  const holders = new Map();
  // Written name -> [{node (the name / identifier), values, group}] (attributed on demand).
  const writesByName = new Map();
  // Element writes (`o[k] = v`): attributed on first need, per group.
  let elementWrites = [];
  const poisonedNames = new Set(), poisonedContainers = new Set();
  const attributedNames = new Set();
  // Member name -> the derived classes (`extends`) declaring it ({node, group}): a holder of
  // that name in one of their ancestors may be shadowed on the receiver, so it gives nothing.
  const derivedMembers = new Map();
  const extraValues = new Map();
  let complete = true;

  const unknown = {unknown: true}, none = {none: true};
  const onlyNone = values => values.every(v => v.none);
  const valuesOf = (expr, group, out) => {
    const e = unwrap(expr);
    if (!e) { out.push(unknown); return out; }
    if (e.kind === K.ArrowFunction || e.kind === K.FunctionExpression) {
      const id = declarations.get(key(e));
      out.push(id ? {fn: id} : unknown);
    } else if (literalKinds.has(e.kind) || (e.kind === K.Identifier && e.text === 'undefined')) out.push(none);
    else if (e.kind === K.ConditionalExpression) { valuesOf(e.whenTrue, group, out); valuesOf(e.whenFalse, group, out); }
    else if (e.kind === K.BinaryExpression && (e.operatorToken.kind === K.QuestionQuestionToken || e.operatorToken.kind === K.BarBarToken)) {
      valuesOf(e.left, group, out); valuesOf(e.right, group, out);
    } else if (e.kind === K.Identifier) out.push({alias: e, group});
    else if (e.kind === K.PropertyAccessExpression) out.push({alias: e.name, group});
    else if (e.kind === K.CallExpression) out.push({call: e, group});
    else out.push(unknown);
    return out;
  };
  const nameText = n => n.text ?? n.getText(n.getSourceFile());
  for (const group of groups) {
    for (const sf of group.sources) {
      if (!fileOf(sf)) continue;
      const write = (target, values) => {
        const t = unwrap(target);
        if (!t) return;
        if (t.kind === K.ObjectLiteralExpression || t.kind === K.ArrayLiteralExpression) { destructured(t); return; }
        if (onlyNone(values)) return;
        const node = t.kind === K.Identifier ? t : t.kind === K.PropertyAccessExpression ? t.name : null;
        if (node) {
          const name = nameText(node);
          let list = writesByName.get(name);
          if (!list) { list = []; writesByName.set(name, list); }
          list.push({node, values, group});
        } else if (t.kind === K.ElementAccessExpression && t.argumentExpression) elementWrites.push({target: t, values, group});
      };
      // Every target of a destructuring assignment gets an unknown value.
      const destructured = pattern => {
        const elements = pattern.kind === K.ObjectLiteralExpression ? pattern.properties : pattern.elements;
        for (const el of elements ?? []) {
          if (el.kind === K.ShorthandPropertyAssignment) write(el.name, [unknown]);
          else if (el.kind === K.PropertyAssignment) write(el.initializer, [unknown]);
          else if (el.kind === K.SpreadAssignment || el.kind === K.SpreadElement) write(el.expression, [unknown]);
          else if (el.kind === K.BinaryExpression && el.operatorToken.kind === K.EqualsToken) write(el.left, [unknown]);
          else if (el.kind !== K.OmittedExpression) write(el, [unknown]);
        }
      };
      const hold = (n, values, owner, container = owner) => { holders.set(key(n), {name: nameText(n.name), owner, container, values}); };
      (function walk(n) {
        if (n.kind === K.VariableDeclaration && n.name?.kind === K.Identifier) {
          const list = n.parent, statement = list?.parent;
          const bound = list?.kind === K.CatchClause || statement?.kind === K.ForOfStatement || statement?.kind === K.ForInStatement;
          hold(n, bound ? [unknown] : n.initializer ? valuesOf(n.initializer, group, []) : [], null);
        } else if (n.kind === K.PropertyDeclaration && n.name && (n.name.kind === K.Identifier || n.name.kind === K.PrivateIdentifier || n.name.kind === K.StringLiteral)) {
          const c = n.parent;
          hold(n, n.initializer ? valuesOf(n.initializer, group, []) : [], c && (c.kind === K.ClassDeclaration || c.kind === K.ClassExpression) ? key(c) : null);
        } else if (n.kind === K.PropertyAssignment && n.name && (n.name.kind === K.Identifier || n.name.kind === K.StringLiteral) && n.parent?.kind === K.ObjectLiteralExpression && !destructuringTarget(n.parent)) {
          hold(n, valuesOf(n.initializer, group, []), null, key(n.parent));
        } else if ((n.kind === K.ClassDeclaration || n.kind === K.ClassExpression) && (n.heritageClauses ?? []).some(c => c.token === K.ExtendsKeyword)) {
          for (const m of n.members ?? []) {
            if (!m.name || m.kind === K.Constructor) continue;
            const name = nameText(m.name);
            let l = derivedMembers.get(name);
            if (!l) { l = []; derivedMembers.set(name, l); }
            l.push({node: n, group});
          }
        } else if (n.kind === K.BinaryExpression && n.operatorToken.kind >= K.FirstAssignment && n.operatorToken.kind <= K.LastAssignment) {
          write(n.left, assignValue.has(n.operatorToken.kind) ? valuesOf(n.right, group, []) : [unknown]);
        }
        n.forEachChild(walk);
      })(sf);
    }
  }

  function destructuringTarget(literal) {
    for (let p = literal.parent, c = literal; p; c = p, p = p.parent) {
      if (p.kind === K.BinaryExpression) return p.left === c && p.operatorToken.kind === K.EqualsToken;
      if (p.kind !== K.PropertyAssignment && p.kind !== K.ObjectLiteralExpression && p.kind !== K.ArrayLiteralExpression && p.kind !== K.ParenthesizedExpression) return false;
    }
    return false;
  }
  // The repository declarations a symbol (aliases followed) stands for, or null.
  function declarationKeys(sym, group) {
    if (!sym) return null;
    const project = group.project, checker = project.checker;
    let s = sym;
    if (s.flags & SymbolFlags.Alias) {
      try { const a = checker.getAliasedSymbol(s); s = a && !checker.isUnknownSymbol(a) ? a : null; } catch { s = null; }
    }
    if (!s) return null;
    const handles = s.declarations ?? [];
    const keys = handles.map(h => h.resolve(project)).filter(d => d && fileOf(d)).map(key);
    return keys.length && keys.length === handles.length ? keys : null;
  }
  function failed(where, e) {
    diagnostics.push({kind: 'assigned_values_incomplete', message: where + ': writes not attributed (' + e + '); the assigned-function rule is off'});
    complete = false;
  }
  function addValues(keys, values) {
    for (const k of keys) { let l = extraValues.get(k); if (!l) { l = []; extraValues.set(k, l); } l.push(...values); }
  }
  // Attribute the writes of `name` (batched per project).
  function attributeName(name) {
    if (attributedNames.has(name)) return;
    attributedNames.add(name);
    const byGroup = new Map();
    for (const w of writesByName.get(name) ?? []) { let l = byGroup.get(w.group); if (!l) { l = []; byGroup.set(w.group, l); } l.push(w); }
    for (const [group, writes] of byGroup) {
      let syms;
      try { syms = group.project.checker.getSymbolAtLocation(writes.map(w => w.node)); } catch (e) { failed(name, e); return; }
      writes.forEach((w, i) => {
        const keys = declarationKeys(syms[i], group);
        if (keys) addValues(keys, w.values);
        else if (!syms[i]) poisonedNames.add(name);
      });
    }
  }
  // Attribute every element write once (object and key types batched per project).
  function attributeElements() {
    if (!elementWrites) return;
    const pending = elementWrites;
    elementWrites = null;
    const byGroup = new Map();
    for (const w of pending) { let l = byGroup.get(w.group); if (!l) { l = []; byGroup.set(w.group, l); } l.push(w); }
    for (const [group, writes] of byGroup) {
      const {checker} = group.project;
      let types;
      try { types = checker.getTypeAtLocation(writes.flatMap(w => [w.target.expression, w.target.argumentExpression])); }
      catch (e) { failed('element writes', e); return; }
      writes.forEach((w, i) => {
        const objectType = types[2 * i], names = literalNames(types[2 * i + 1]);
        if (!names) {
          const parts = objectType?.isUnionType?.() ? objectType.getTypes() ?? [] : [objectType];
          for (const part of parts) {
            for (const h of part?.getSymbol?.()?.declarations ?? []) {
              const decl = h.resolve(group.project);
              if (decl && containerKinds.has(decl.kind) && fileOf(decl)) poisonedContainers.add(key(decl));
            }
          }
          return;
        }
        for (const name of names) {
          let prop = null;
          try {
            prop = objectType ? checker.getPropertyOfType(objectType, name) : null;
            if (!prop && objectType) { const apparent = checker.getApparentType(objectType); prop = apparent ? checker.getPropertyOfType(apparent, name) : null; }
          } catch { prop = null; }
          const keys = prop ? declarationKeys(prop, group) : null;
          if (keys) addValues(keys, w.values);
          else if (!prop) poisonedNames.add(name);
        }
      });
    }
  }
  function literalNames(type) {
    if (!type) return null;
    if (type.isStringLiteralType?.()) return [String(type.value)];
    if (type.isUnionType?.()) {
      const parts = type.getTypes() ?? [];
      if (!parts.length || !parts.every(t => t.isStringLiteralType?.())) return null;
      return parts.map(t => String(t.value));
    }
    return null;
  }
  function aliasKeys(v) {
    if (v.keys === undefined) {
      let sym = null;
      try { sym = v.group.project.checker.getSymbolAtLocation(v.alias); } catch { sym = null; }
      v.keys = declarationKeys(sym, v.group);
    }
    return v.keys;
  }

  // Whether the derived class `d` ({node, group}) has the class `owner` (key) among its
  // ancestors (checker base types); true when the checker cannot tell.
  function derivesFrom(d, owner) {
    const project = d.group.project, checker = project.checker;
    try {
      let level = [checker.getTypeAtLocation(d.node.name ?? d.node)];
      for (let i = 0; i < 16 && level.length; i++) {
        const next = [];
        for (const t of level) {
          for (const b of (t?.isClassOrInterface?.() ? checker.getBaseTypes(t) : t?.getBaseTypes?.()) ?? []) {
            const decls = b.getSymbol?.()?.declarations ?? [];
            if (decls.some(h => { const n = h.resolve(project); return n && key(n) === owner; })) return true;
            next.push(b.getTarget?.() ?? b);
          }
        }
        level = next;
      }
      return false;
    } catch { return true; }
  }

  const memo = new Map();
  // The one repository function the declaration `k` holds (or is), or null.
  function functionOfKey(k, depth) {
    const direct = declarations.get(k);
    if (direct) return direct;
    if (memo.has(k)) return memo.get(k);
    const h = holders.get(k);
    if (!h || depth > MAX_DEPTH) return null;
    memo.set(k, null); // a cycle gives nothing
    attributeName(h.name);
    attributeElements();
    const shadowed = h.owner && (derivedMembers.get(h.name) ?? []).some(d => key(d.node) !== h.owner && derivesFrom(d, h.owner));
    const result = complete && !shadowed && !poisonedNames.has(h.name) && !(h.container && poisonedContainers.has(h.container))
      ? functionOfValues([...h.values, ...(extraValues.get(k) ?? [])], depth) : null;
    memo.set(k, result);
    return result;
  }

  // The one repository function a list of values gives, or null.
  function functionOfValues(values, depth) {
    const found = new Set();
    for (const v of values) {
      if (v.none) continue;
      let id = null;
      if (v.fn) id = v.fn;
      else if (v.alias) {
        const keys = aliasKeys(v);
        const targets = keys ? new Set(keys.map(a => functionOfKey(a, depth + 1))) : null;
        id = targets && targets.size === 1 ? [...targets][0] : null;
      } else if (v.call) id = returnedFunction(v.call, v.group, depth + 1);
      if (!id) return null;
      found.add(id);
    }
    return found.size === 1 ? [...found][0] : null;
  }
  // The one repository function a call returns: the checker resolves the callee to a
  // repository function with a body (neither async nor a generator: those return a promise /
  // an iterator) whose every return value - its expression body, or the `return` statements
  // of its own body (nested functions excluded) - is that function.
  const returned = new Map();
  function returnedFunction(call, group, depth) {
    if (depth > MAX_DEPTH) return null;
    let decl = null;
    try { decl = group.project.checker.getResolvedSignature(call)?.declaration?.resolve(group.project) ?? null; } catch { decl = null; }
    if (!decl || !decl.body || !fileOf(decl) || !declarations.has(key(decl))) return null;
    const k = key(decl);
    if (returned.has(k)) return returned.get(k);
    returned.set(k, null); // recursion gives nothing
    const result = functionOfValues(returnValues(decl, group), depth);
    returned.set(k, result);
    return result;
  }
  function returnValues(fn, group) {
    if (fn.asteriskToken || (fn.modifiers ?? []).some(m => m.kind === K.AsyncKeyword)) return [unknown];
    if (fn.body.kind !== K.Block) return valuesOf(fn.body, group, []);
    const out = [];
    (function walk(n) {
      if (n.kind === K.ReturnStatement) { if (n.expression) valuesOf(n.expression, group, out); else out.push(none); }
      if (nested.has(n.kind)) return;
      n.forEachChild(walk);
    })(fn.body);
    return out;
  }

  return {
    // The function a call's callee value holds when that callee is itself a call
    // (`compose(stack)(context)`), or null.
    returnedFunction(call, project) {
      return complete ? returnedFunction(call, {project}, 0) : null;
    },
    functionOf(sym, project) {
      if (!sym) return null;
      const decls = (sym.declarations ?? []).map(h => h.resolve(project));
      if (!decls.length || decls.some(d => !d || !fileOf(d) || !holders.has(key(d)))) return null;
      const ids = new Set(decls.map(d => functionOfKey(key(d), 0)));
      return complete && ids.size === 1 && !ids.has(null) ? [...ids][0] : null;
    }
  };
}
