// Function-type rule inside the TypeScript checker (owner tables; DESIGN §1.10 item 3).
//
// For a callback argument of a call resolved to a declaration outside the repository
// (analyze.mjs calls this for Arrow/FunctionExpression/Identifier/PropertyAccess arguments of
// every `external_signature` call), the declared parameter type and whether it is a function
// type:
//   {source (repository-relative file), call:{start,end} (callee byte span),
//    arg:{start,end} (argument byte span; the member name for `obj.method`), param_name,
//    param_type, verdict ('function_type'|'not_function_type'|'top_type'|'unknown'),
//    route:'checker', library_symbol}
// Rules (checker facts, no tables):
// * the parameter type is the argument's contextual type (covers overloads: the checker's
//   resolved signature), else the resolved signature's parameter type at the index;
// * function type = a type with call signatures, the global `Function` interface (which has
//   no call signatures of its own), or a union / intersection containing one;
// * `any` / `unknown` / `Object` / `object` (and unions with them but without a function) are
//   top types: the function is accepted as a value (`console.log(work)`);
// * a type parameter is classified by its apparent type (constraint), an unconstrained one is
//   a top type.
// Byte offsets are UTF-8 (the Rust side's spans), converted from the checker's UTF-16
// positions per source file.

const CALL_SIGNATURES = 0; // SignatureKind.Call

// Keyed by the source-file object: an edited file is a new object in the next snapshot.
const offsetTables = new WeakMap();

/** UTF-16 position -> UTF-8 byte offset table of a source file (null for ASCII files). */
function offsetTable(sf) {
  if (offsetTables.has(sf)) return offsetTables.get(sf);
  const text = sf.text;
  let table = null;
  if (Buffer.byteLength(text, 'utf8') !== text.length) {
    table = new Uint32Array(text.length + 1);
    let b = 0;
    for (let i = 0; i < text.length; i++) {
      table[i] = b;
      const c = text.charCodeAt(i);
      if (c < 0x80) b += 1;
      else if (c < 0x800) b += 2;
      else if (c >= 0xD800 && c <= 0xDBFF) { table[i + 1] = b; b += 4; i++; }
      else b += 3;
    }
    table[text.length] = b;
  }
  offsetTables.set(sf, table);
  return table;
}

function byteOf(sf, pos) {
  const t = offsetTable(sf);
  return t ? t[pos] : pos;
}

function span(sf, node) {
  return {start: byteOf(sf, node.getStart(sf)), end: byteOf(sf, node.end)};
}

function isTop(t, TypeFlags) {
  if (t.flags & (TypeFlags.Any | TypeFlags.Unknown | TypeFlags.NonPrimitive)) return true;
  const name = t.getSymbol?.()?.name;
  return name === 'Object';
}

function isFunctionLike(checker, t) {
  try {
    if (checker.getSignaturesOfType(t, CALL_SIGNATURES).length > 0) return true;
  } catch { /* not an object type */ }
  const name = t.getSymbol?.()?.name;
  return name === 'Function' || name === 'CallableFunction' || name === 'NewableFunction';
}

/** Verdict of a declared parameter type. */
function classify(checker, TypeFlags, t, depth = 0) {
  if (!t || depth > 8) return 'unknown';
  if (t.flags & (TypeFlags.Any | TypeFlags.Unknown)) return 'top_type';
  if (isFunctionLike(checker, t)) return 'function_type';
  let parts;
  try { parts = t.getTypes?.(); } catch { parts = undefined; }
  if (parts && parts.length) {
    const verdicts = parts.map(p => classify(checker, TypeFlags, p, depth + 1));
    if (verdicts.includes('function_type')) return 'function_type';
    if (verdicts.includes('top_type')) return 'top_type';
    if (verdicts.every(v => v === 'unknown')) return 'unknown';
    return 'not_function_type';
  }
  if (t.flags & TypeFlags.TypeParameter) {
    let apparent;
    try { apparent = checker.getApparentType(t); } catch { apparent = undefined; }
    if (!apparent || apparent === t) return 'top_type';
    const v = classify(checker, TypeFlags, apparent, depth + 1);
    return v === 'function_type' ? 'function_type' : 'top_type';
  }
  if (isTop(t, TypeFlags)) return 'top_type';
  return 'not_function_type';
}

/** `Array.map`, `util.promisify`, `setTimeout`: containers of a library declaration. */
function qualifiedName(K, d) {
  const names = [];
  for (let n = d; n; n = n.parent) {
    if (!n.name) continue;
    if (n === d || n.kind === K.ClassDeclaration || n.kind === K.InterfaceDeclaration || n.kind === K.ModuleDeclaration) {
      let text;
      // `text` of an identifier or a string-literal module name (`declare module "util"`) is unquoted.
      try { text = n.name.text; } catch { text = undefined; }
      if (text) names.push(text);
    }
  }
  return names.length ? names.reverse().join('.') : null;
}

export function callbackParams(ctx, call, argIndex) {
  const {checker, project, K, TypeFlags, norm, original} = ctx;
  if (!checker || !call || !call.arguments) return null;
  const arg = call.arguments[argIndex];
  if (!arg) return null;
  const sf = call.getSourceFile();
  const source = original.get(norm(sf.fileName));
  if (!source) return null;

  let signature;
  try { signature = checker.getResolvedSignature(call); } catch { signature = undefined; }
  let type;
  try { type = checker.getContextualType(arg); } catch { type = undefined; }
  if (!type && signature) {
    try { type = checker.getParameterType(signature, argIndex); } catch { type = undefined; }
  }

  let paramName = null;
  let librarySymbol = null;
  if (signature) {
    try {
      const params = signature.getParameters();
      const p = params[Math.min(argIndex, params.length - 1)];
      if (p) paramName = p.name;
    } catch { /* no parameter symbols */ }
    try {
      const d = signature.declaration?.resolve?.(project);
      if (d) librarySymbol = qualifiedName(K, d);
    } catch { /* declaration outside the program */ }
  }

  const verdict = classify(checker, TypeFlags, type);
  let paramType = '';
  try { paramType = type ? checker.typeToString(type) : ''; } catch { paramType = ''; }
  const node = arg.kind === K.PropertyAccessExpression && arg.name ? arg.name : arg;
  return {
    source,
    call: span(sf, call.expression),
    arg: span(sf, node),
    param_name: paramName,
    param_type: paramType,
    verdict,
    route: 'checker',
    library_symbol: librarySymbol,
  };
}
