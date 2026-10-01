// Library call locations (owner derive; DESIGN §1.10 item 2, §4.13 task 6). For a call whose
// callee resolves to a declaration outside the repository:
//   {source (repository-relative file), at:{start,end} (callee byte span), line (1-based),
//    file (index into the output's library_files), decl_line (0-based), decl_column (UTF-8
//    byte column of the declaration name), symbol (null: trace-library names it)}
// and the library file interned into `files` (the output's library_files):
//   {path, package, version, stdlib, readable, language}
// Rules (paths and package metadata only, read structurally):
// * package = the directory after the last `node_modules` (`@scope/name` for scoped
//   packages), version from its package.json;
// * the compiler's own `lib.*.d.ts` files are the JavaScript standard library
//   (package `javascript-stdlib`, version = the TypeScript version);
// * readable = a `.js`/`.mjs`/`.cjs` file, or a declaration file whose implementation is
//   installed (trace-library derives from the implementation, same rules as
//   trace-library adapters/javascript.rs `implementation_of_declaration`): the sibling
//   `.js`/`.mjs`/`.cjs`; the package manifest's declaration entry mirrored onto its
//   implementation entry (`types: dist/types/index.d.ts` + `main: dist/index.js`:
//   `dist/types/x.d.ts` is `dist/x.js`); a `@types/<name>` file -> the same path (or the
//   manifest entry for `index`) in the package `<name>` next to it;
// * files in no package (outside node_modules, not the compiler's lib) are not library calls.
import fs from 'node:fs';
import path from 'node:path';

const packageVersions = new Map();

function packageJsonVersion(dir) {
  if (packageVersions.has(dir)) return packageVersions.get(dir);
  let version = null;
  try {
    const json = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8'));
    if (typeof json.version === 'string') version = json.version;
  } catch { version = null; }
  packageVersions.set(dir, version);
  return version;
}

function languageOf(file) {
  if (file.endsWith('.tsx')) return 'tsx';
  if (['.ts', '.mts', '.cts'].some(ext => file.endsWith(ext))) return 'typescript';
  return 'javascript';
}

function isFile(p) {
  try { return fs.statSync(p).isFile(); } catch { return false; }
}

const IMPLEMENTATION_EXTENSIONS = ['.js', '.mjs', '.cjs'];
const DECLARATION_SUFFIXES = ['.d.ts', '.d.mts', '.d.cts'];
const IMPLEMENTATION_CONDITIONS = ['import', 'require', 'default', 'node', 'module', 'main'];

function withImplementationExtension(base) {
  for (const ext of IMPLEMENTATION_EXTENSIONS) if (isFile(base + ext)) return base + ext;
  return null;
}

function readManifest(dir) {
  try { return JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8')); } catch { return null; }
}

function cleanRel(text) {
  return String(text).replace(/\\/g, '/').replace(/^\.\//, '');
}

/** [declaration entry, implementation entry] pairs of a package manifest. */
function manifestPairs(manifest) {
  const out = [];
  const walk = (value, depth) => {
    if (!value || typeof value !== 'object' || Array.isArray(value) || depth > 6) return;
    const types = typeof value.types === 'string' ? value.types : (typeof value.typings === 'string' ? value.typings : null);
    if (types) {
      for (const key of IMPLEMENTATION_CONDITIONS) {
        if (typeof value[key] === 'string') out.push([cleanRel(types), cleanRel(value[key])]);
      }
    }
    for (const [key, child] of Object.entries(value)) {
      if (key.startsWith('.') || IMPLEMENTATION_CONDITIONS.includes(key) || key === 'exports') walk(child, depth + 1);
    }
  };
  walk(manifest, 0);
  return out;
}

/** {dir, name} of the installed package holding a file (node_modules/<name>, node_modules/@scope/<name>). */
function packageDir(file) {
  const parts = file.split(/[\\/]/);
  const i = parts.lastIndexOf('node_modules');
  if (i < 0 || i + 1 >= parts.length) return null;
  const scoped = parts[i + 1].startsWith('@');
  const depth = scoped ? i + 3 : i + 2;
  if (depth > parts.length - 1) return null;
  return {dir: parts.slice(0, depth).join(path.sep), name: parts.slice(i + 1, depth).join('/'), rest: parts.slice(depth)};
}

/** The installed implementation of a declaration file, or null. */
export function implementationOf(file) {
  const stub = DECLARATION_SUFFIXES.find(ext => file.endsWith(ext));
  if (!stub) return null;
  const base = file.slice(0, file.length - stub.length);
  const sibling = withImplementationExtension(base);
  if (sibling) return sibling;
  const pkg = packageDir(file);
  if (!pkg) return null;
  const restParts = pkg.rest.slice();
  const last = restParts.pop();
  if (last === undefined) return null;
  const stem = last.slice(0, last.length - stub.length);
  const rel = [...restParts, stem].join('/');
  if (pkg.name.startsWith('@types/')) {
    const described = pkg.name.slice('@types/'.length);
    const target = described.includes('__') ? '@' + described.replace('__', '/') : described;
    const modules = path.dirname(path.dirname(pkg.dir));
    const targetDir = path.join(modules, ...target.split('/'));
    const direct = withImplementationExtension(path.join(targetDir, ...rel.split('/')));
    if (direct) return direct;
    if (rel === 'index') {
      const manifest = readManifest(targetDir);
      const main = cleanRel((manifest && (manifest.main || manifest.module)) || 'index.js');
      const entry = path.join(targetDir, ...main.split('/'));
      if (isFile(entry)) return entry;
      return withImplementationExtension(entry) ?? (isFile(path.join(entry, 'index.js')) ? path.join(entry, 'index.js') : null);
    }
    return null;
  }
  const manifest = readManifest(pkg.dir);
  if (!manifest) return null;
  for (const [types, implementation] of manifestPairs(manifest)) {
    const typesDir = types.includes('/') ? types.slice(0, types.lastIndexOf('/')) : '';
    const implDir = implementation.includes('/') ? implementation.slice(0, implementation.lastIndexOf('/')) : '';
    let inner = null;
    if (!typesDir) inner = rel;
    else if (rel.startsWith(typesDir + '/')) inner = rel.slice(typesDir.length + 1);
    if (inner === null) continue;
    const target = implDir ? path.join(pkg.dir, ...implDir.split('/'), ...inner.split('/')) : path.join(pkg.dir, ...inner.split('/'));
    const found = withImplementationExtension(target);
    if (found) return found;
  }
  return null;
}

/** Whether trace-library can derive from a library file (its source or its implementation). */
export function readableLibraryFile(file) {
  if (IMPLEMENTATION_EXTENSIONS.some(ext => file.endsWith(ext))) return isFile(file);
  return implementationOf(file) !== null;
}

function readable(file) {
  return readableLibraryFile(file);
}

/** {package, version, stdlib} of a library file path, or null. */
export function packageOf(file) {
  const parts = file.split(/[\\/]/);
  const i = parts.lastIndexOf('node_modules');
  if (i < 0 || i + 1 >= parts.length) return null;
  const first = parts[i + 1];
  const leaf = parts[parts.length - 1];
  if (first === 'typescript' && parts[i + 2] === 'lib' && leaf.startsWith('lib.') && leaf.endsWith('.d.ts')) {
    return {package: 'javascript-stdlib', version: packageJsonVersion(parts.slice(0, i + 2).join(path.sep)), stdlib: true};
  }
  const name = first.startsWith('@') && i + 2 < parts.length ? first + '/' + parts[i + 2] : first;
  const depth = first.startsWith('@') ? i + 3 : i + 2;
  return {package: name, version: packageJsonVersion(parts.slice(0, depth).join(path.sep)), stdlib: false};
}

// UTF-16 position -> UTF-8 byte offset per source file (null table for ASCII files), keyed by
// the source-file object (an edited file is a new object in the next snapshot).
const offsetTables = new WeakMap();

function byteOf(sf, pos) {
  let table = offsetTables.get(sf);
  if (table === undefined) {
    const text = sf.text;
    table = null;
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
  }
  return table ? table[pos] : pos;
}

export function libraryCall(ctx, call, declaration, files) {
  if (!Array.isArray(files) || !declaration) return null;
  const sf = call.getSourceFile();
  const source = ctx.original.get(ctx.norm(sf.fileName));
  if (!source) return null;
  const dsf = declaration.getSourceFile();
  const file = path.resolve(dsf.fileName);
  const pkg = packageOf(file);
  if (!pkg) return null;
  const entry = {
    path: file,
    package: pkg.package,
    version: pkg.version,
    stdlib: pkg.stdlib,
    readable: readable(file),
    language: languageOf(file),
  };
  let index = files.findIndex(f => f.path === entry.path);
  if (index < 0) { files.push(entry); index = files.length - 1; }
  const callee = call.tag ?? call.expression ?? call;
  const start = callee.getStart(sf);
  const nameNode = declaration.name ?? declaration;
  const pos = nameNode.getStart(dsf);
  const lc = dsf.getLineAndCharacterOfPosition(pos);
  const lineStart = dsf.getPositionOfLineAndCharacter(lc.line, 0);
  return {
    source,
    at: {start: byteOf(sf, start), end: byteOf(sf, callee.end)},
    line: sf.getLineAndCharacterOfPosition(start).line + 1,
    file: index,
    decl_line: lc.line,
    decl_column: byteOf(dsf, pos) - byteOf(dsf, lineStart),
    symbol: null,
  };
}
