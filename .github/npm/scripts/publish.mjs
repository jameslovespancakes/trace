// Publish the packages written by pack.mjs: every platform package first, then the launcher
// (whose optionalDependencies name them), with npm provenance. A version already on the
// registry is skipped, so a re-run of the release job is safe. A prerelease published under
// another tag also becomes `latest` while no stable version exists, so
// `npm install -g <package>` installs it.
//
//   NODE_AUTH_TOKEN=<npm token> node .github/npm/scripts/publish.mjs <out dir> [--tag latest]
//   node .github/npm/scripts/publish.mjs <out dir> --dry-run      (no token, nothing published)

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";

const NPM = process.platform === "win32" ? "npm.cmd" : "npm";
const PRERELEASE = /^\d+\.\d+\.\d+-/;

function npm(args, options = {}) {
  return execFileSync(NPM, args, { encoding: "utf8", ...options });
}

function published(name, version) {
  try {
    return npm(["view", `${name}@${version}`, "version"], { stdio: ["ignore", "pipe", "ignore"] }).trim() === version;
  } catch {
    return false;
  }
}

function latest(name) {
  try {
    return npm(["view", name, "dist-tags.latest"], { stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return "";
  }
}

function packages(out) {
  return readdirSync(resolve(out), { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .map((d) => d.name)
    // Platform packages first; the launcher (`trace`) last.
    .sort((a, b) => (a === "trace") - (b === "trace") || a.localeCompare(b))
    .map((d) => {
      const dir = join(resolve(out), d);
      const { name, version } = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
      return { dir, name, version, launcher: d === "trace" };
    });
}

function main() {
  const argv = process.argv.slice(2);
  const out = argv[0];
  if (!out || out.startsWith("--")) throw new Error("Usage: node publish.mjs <out dir> [--tag <dist-tag>] [--dry-run]");
  const dryRun = argv.includes("--dry-run");
  const tagAt = argv.indexOf("--tag");
  const tag = tagAt >= 0 ? argv[tagAt + 1] : "latest";
  if (!dryRun && !process.env.NODE_AUTH_TOKEN) {
    throw new Error("Set the NPM_TOKEN repository secret (an npm token with publish rights) to publish.");
  }
  for (const p of packages(out)) {
    if (dryRun) {
      npm(["publish", "--dry-run", "--access", "public", "--tag", tag], { cwd: p.dir, stdio: "inherit" });
      console.log(`dry run ${p.name}@${p.version}`);
      continue;
    }
    if (published(p.name, p.version)) {
      console.log(`skip ${p.name}@${p.version} (already published)`);
    } else {
      npm(["publish", "--access", "public", "--provenance", "--tag", tag], { cwd: p.dir, stdio: "inherit" });
      console.log(`published ${p.name}@${p.version} (${tag})`);
    }
    if (p.launcher && tag !== "latest") {
      const current = latest(p.name);
      if (!current || PRERELEASE.test(current)) {
        npm(["dist-tag", "add", `${p.name}@${p.version}`, "latest"], { stdio: "inherit" });
        console.log(`${p.name}@${p.version} is also latest (no stable version yet)`);
      }
    }
  }
}

try {
  main();
} catch (err) {
  console.error(`Error: ${err.message}`);
  process.exit(1);
}
