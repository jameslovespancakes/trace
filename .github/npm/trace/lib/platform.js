"use strict";
// Which native trace binary this machine needs, and where npm put it.
//
// The main package (`@scope/trace`) lists one package per platform as optionalDependencies
// (`@scope/trace-win32-x64`, ...). Each declares `os` / `cpu`, so npm installs only the one
// matching this machine; it holds the binary in `bin/`. No install scripts run.

const fs = require("node:fs");
const path = require("node:path");

/** npm platform key (`process.platform-process.arch`) -> Rust target of the release build. */
const TARGETS = {
  "win32-x64": "x86_64-pc-windows-msvc",
  "win32-arm64": "aarch64-pc-windows-msvc",
  "linux-x64": "x86_64-unknown-linux-musl",
  "linux-arm64": "aarch64-unknown-linux-musl",
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
};

function platformKey(platform = process.platform, arch = process.arch) {
  return `${platform}-${arch}`;
}

/** `@scope/trace` + `linux-x64` -> `@scope/trace-linux-x64`. */
function packageName(mainName, key) {
  return `${mainName}-${key}`;
}

function binaryName(platform = process.platform) {
  return platform === "win32" ? "trace.exe" : "trace";
}

/**
 * Absolute path of the native binary. `TRACE_BINARY` overrides it (development builds).
 * Throws an Error with a one-line message when the binary is missing.
 */
function binaryPath(options = {}) {
  const env = options.env ?? process.env;
  if (env.TRACE_BINARY) {
    return env.TRACE_BINARY;
  }
  const platform = options.platform ?? process.platform;
  const arch = options.arch ?? process.arch;
  const mainName = options.mainName ?? require("../package.json").name;
  const resolve = options.resolve ?? require.resolve;
  const key = platformKey(platform, arch);
  if (!TARGETS[key]) {
    throw new Error(`trace has no build for ${key}. Supported: ${Object.keys(TARGETS).join(", ")}.`);
  }
  const pkg = packageName(mainName, key);
  let dir;
  try {
    dir = path.dirname(resolve(`${pkg}/package.json`));
  } catch {
    throw new Error(
      `The trace binary for ${key} is missing (${pkg}). Reinstall without --omit=optional: npm install -g ${mainName}`,
    );
  }
  const bin = path.join(dir, "bin", binaryName(platform));
  if (!(options.exists ?? fs.existsSync)(bin)) {
    throw new Error(`The trace binary is missing from ${pkg}. Reinstall: npm install -g ${mainName}`);
  }
  return bin;
}

module.exports = { TARGETS, platformKey, packageName, binaryName, binaryPath };
