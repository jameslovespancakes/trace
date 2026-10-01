// Defense in depth for the isolated analysis process; not an OS sandbox.
const fs = require('node:fs');
const path = require('node:path');
const child = require('node:child_process');
const net = require('node:net');
const tls = require('node:tls');
const http = require('node:http');
const https = require('node:https');
// Writes are allowed below the workspace (CODEPATH_LSP_WRITE_ROOT) and below the extra roots
// in CODEPATH_LSP_WRITE_ROOTS (path-list separated; trace's per-repository state dir, e.g. the
// Intelephense index storage). Everything else is refused.
const allowedRoots = [process.env.CODEPATH_LSP_WRITE_ROOT]
  .concat(String(process.env.CODEPATH_LSP_WRITE_ROOTS || '').split(path.delimiter))
  .filter((root) => typeof root === 'string' && root.length > 0 && path.isAbsolute(root))
  .map((root) => path.resolve(root));
if (allowedRoots.length === 0) throw new Error('Codepath: CODEPATH_LSP_WRITE_ROOT is not set');
function inside(root, actual) {
  const relative = path.relative(root, actual);
  return !(relative === '..' || relative.startsWith('..' + path.sep) || path.isAbsolute(relative));
}
function check(p) {
  if (typeof p === 'number') return;
  const actual = path.resolve(p instanceof URL ? require('node:url').fileURLToPath(p) : String(p));
  if (!allowedRoots.some((root) => inside(root, actual))) {
    throw new Error('Codepath: write outside analysis workspace refused');
  }
}
const refuse = () => { throw new Error('Codepath: network/process execution disabled in analyzer'); };
for (const key of ['spawn', 'spawnSync', 'exec', 'execSync', 'execFile', 'execFileSync', 'fork']) child[key] = refuse;
net.Socket.prototype.connect = refuse;
for (const key of ['connect', 'createConnection']) net[key] = refuse;
tls.connect = refuse;
for (const mod of [http, https]) for (const key of ['request', 'get']) mod[key] = refuse;
for (const key of ['writeFile', 'writeFileSync', 'appendFile', 'appendFileSync', 'mkdir', 'mkdirSync', 'mkdtemp', 'mkdtempSync', 'unlink', 'unlinkSync', 'rm', 'rmSync', 'rmdir', 'rmdirSync', 'truncate', 'truncateSync', 'chmod', 'chmodSync']) {
  const original = fs[key];
  if (original) fs[key] = function(p, ...rest) { check(p); return original.call(this, p, ...rest); };
}
for (const key of ['rename', 'renameSync', 'link', 'linkSync', 'symlink', 'symlinkSync']) {
  const original = fs[key];
  fs[key] = function(a, b, ...rest) { check(a); check(b); return original.call(this, a, b, ...rest); };
}
for (const key of ['open', 'openSync']) {
  const original = fs[key];
  fs[key] = function(p, flags, ...rest) {
    if (typeof flags === 'string' ? flags.includes('w') || flags.includes('a') || flags.includes('+') : flags & (fs.constants.O_WRONLY | fs.constants.O_RDWR | fs.constants.O_CREAT | fs.constants.O_TRUNC | fs.constants.O_APPEND)) check(p);
    return original.call(this, p, flags, ...rest);
  };
}
for (const key of ['writeFile', 'appendFile', 'mkdir', 'mkdtemp', 'unlink', 'rm', 'rmdir', 'truncate', 'chmod']) {
  const original = fs.promises[key];
  if (original) fs.promises[key] = function(p, ...rest) { check(p); return original.call(this, p, ...rest); };
}
for (const key of ['rename', 'link', 'symlink']) {
  const original = fs.promises[key];
  fs.promises[key] = function(a, b, ...rest) { check(a); check(b); return original.call(this, a, b, ...rest); };
}
const promiseOpen = fs.promises.open;
fs.promises.open = function(p, flags, ...rest) {
  if (typeof flags === 'string' ? flags.includes('w') || flags.includes('a') || flags.includes('+') : flags & (fs.constants.O_WRONLY | fs.constants.O_RDWR | fs.constants.O_CREAT | fs.constants.O_TRUNC | fs.constants.O_APPEND)) check(p);
  return promiseOpen.call(this, p, flags, ...rest);
};
const createWriteStream = fs.createWriteStream;
fs.createWriteStream = function(p, ...rest) { check(p); return createWriteStream.call(this, p, ...rest); };
