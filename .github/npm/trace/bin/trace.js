#!/usr/bin/env node
"use strict";
// Runs the native trace binary of this machine with the same arguments, stdin, stdout and
// stderr, and exits with its exit code.

const { spawnSync } = require("node:child_process");
const { binaryPath } = require("../lib/platform.js");

let bin;
try {
  bin = binaryPath();
} catch (err) {
  process.stderr.write(`Error: ${err.message}\n`);
  process.exit(3);
}

const result = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  process.stderr.write(`Error: Could not start trace (${bin}): ${result.error.message}\n`);
  process.exit(3);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
}
process.exit(result.status ?? 3);
