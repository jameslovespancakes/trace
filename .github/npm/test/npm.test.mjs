// Unit tests of the npm launcher and packaging (`node --test .github/npm/test`).

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { pack } from "../scripts/pack.mjs";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const platform = require("../trace/lib/platform.js");

test("rule_every_platform_maps_to_a_release_target", () => {
  assert.deepEqual(Object.keys(platform.TARGETS).sort(), [
    "darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64", "win32-arm64", "win32-x64",
  ]);
  assert.equal(platform.packageName("@s/trace", "linux-x64"), "@s/trace-linux-x64");
  assert.equal(platform.binaryName("win32"), "trace.exe");
  assert.equal(platform.binaryName("linux"), "trace");
});

test("rule_binary_path_resolves_the_platform_package", () => {
  const bin = platform.binaryPath({
    env: {},
    platform: "linux",
    arch: "x64",
    mainName: "@s/trace",
    resolve: (id) => {
      assert.equal(id, "@s/trace-linux-x64/package.json");
      return "/nm/@s/trace-linux-x64/package.json";
    },
    exists: () => true,
  });
  assert.equal(bin, join("/nm/@s/trace-linux-x64", "bin", "trace"));
  // TRACE_BINARY wins (development builds).
  assert.equal(platform.binaryPath({ env: { TRACE_BINARY: "/dev/trace" } }), "/dev/trace");
});

test("rule_missing_platform_package_is_one_clear_error", () => {
  const missing = () => platform.binaryPath({
    env: {}, platform: "linux", arch: "x64", mainName: "@s/trace",
    resolve: () => { throw new Error("not found"); },
  });
  assert.throws(missing, /The trace binary for linux-x64 is missing \(@s\/trace-linux-x64\)\. Reinstall without --omit=optional/);
  const unsupported = () => platform.binaryPath({ env: {}, platform: "aix", arch: "ppc64", mainName: "@s/trace" });
  assert.throws(unsupported, /trace has no build for aix-ppc64\. Supported:/);
});

test("rule_pack_writes_one_package_per_platform_and_the_launcher", () => {
  const root = mkdtempSync(join(tmpdir(), "trace-npm-"));
  try {
    const binaries = join(root, "bin");
    for (const [key, target] of Object.entries(platform.TARGETS)) {
      const dir = join(binaries, `bin-${target}`);
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, platform.binaryName(key.split("-")[0])), `binary for ${key}`);
    }
    const out = join(root, "out");
    const packed = pack({ version: "0.9.0-beta.1", binaries, out });
    assert.equal(packed.platforms.length, 6);
    const main = JSON.parse(readFileSync(join(out, "trace", "package.json"), "utf8"));
    assert.equal(main.version, "0.9.0-beta.1");
    assert.equal(main.bin.trace, "bin/trace.js");
    assert.equal(Object.keys(main.optionalDependencies).length, 6);
    for (const v of Object.values(main.optionalDependencies)) assert.equal(v, "0.9.0-beta.1");
    assert.ok(existsSync(join(out, "trace", "lib", "platform.js")));
    assert.ok(main.files.includes("CHANGELOG.md"));
    assert.equal(readFileSync(join(out, "trace", "CHANGELOG.md"), "utf8"), readFileSync(resolve(HERE, "../../..", "CHANGELOG.md"), "utf8"));
    const win = JSON.parse(readFileSync(join(out, "win32-x64", "package.json"), "utf8"));
    assert.equal(win.name, `${main.name}-win32-x64`);
    assert.deepEqual([win.os, win.cpu], [["win32"], ["x64"]]);
    assert.equal(readFileSync(join(out, "win32-x64", "bin", "trace.exe"), "utf8"), "binary for win32-x64");
    if (process.platform !== "win32") {
      assert.ok(statSync(join(out, "linux-x64", "bin", "trace")).mode & 0o111, "executable bit");
    }
    // A release without every binary is refused.
    rmSync(join(binaries, "bin-aarch64-apple-darwin"), { recursive: true });
    assert.throws(() => pack({ version: "0.9.0", binaries, out }), /Missing binary for darwin-arm64/);
    assert.throws(() => pack({ version: "v0.9.0", binaries, out }), /Not a release version/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("rule_launcher_forwards_arguments_and_exit_code", { skip: process.platform === "win32" }, () => {
  const root = mkdtempSync(join(tmpdir(), "trace-npm-run-"));
  try {
    const fake = join(root, "trace");
    writeFileSync(fake, "#!/bin/sh\necho \"args: $*\"\nexit 7\n");
    chmodSync(fake, 0o755);
    const launcher = resolve(HERE, "..", "trace", "bin", "trace.js");
    const r = spawnSync(process.execPath, [launcher, "uses", "a b"], { env: { ...process.env, TRACE_BINARY: fake }, encoding: "utf8" });
    assert.equal(r.stdout, "args: uses a b\n");
    assert.equal(r.status, 7);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
