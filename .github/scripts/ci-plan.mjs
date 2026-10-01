import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { validate } from "./changelog.mjs";

export function workspaceVersion(text = readFileSync("Cargo.toml", "utf8")) {
  const block = text.split("[workspace.package]")[1]?.split(/\n\[/)[0];
  const version = block?.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) throw new Error("Invalid workspace version.");
  return version;
}

export const platforms = [
  { name: "Linux x64", os: "ubuntu-latest", target: "x86_64-unknown-linux-musl", tests: true, lint: true, native: true, archive: "tar.gz" },
  { name: "Windows x64", os: "windows-latest", target: "x86_64-pc-windows-msvc", tests: true, lint: false, native: true, archive: "zip" },
  { name: "macOS ARM64", os: "macos-14", target: "aarch64-apple-darwin", tests: true, lint: false, native: true, archive: "tar.gz" },
  { name: "Linux ARM64", os: "ubuntu-24.04-arm", target: "aarch64-unknown-linux-musl", tests: false, lint: false, native: true, archive: "tar.gz" },
  { name: "Windows ARM64", os: "windows-11-arm", target: "aarch64-pc-windows-msvc", tests: false, lint: false, native: true, archive: "zip" },
  { name: "macOS x64", os: "macos-14", target: "x86_64-apple-darwin", tests: false, lint: false, native: false, archive: "tar.gz" },
];

export function candidate({ event, ref, version, changelog, hasTag }) {
  return event === "push" && ref === "refs/heads/main" && validate(changelog).has(version) && !hasTag(version);
}

export function plan({ event, ref, rustChanged, candidate = false }) {
  const rust = event === "workflow_dispatch" || rustChanged || candidate;
  const release = ref === "refs/heads/main" && ["push", "workflow_dispatch"].includes(event) && (candidate || event === "workflow_dispatch");
  return { rust, release, matrix: { include: platforms.filter(p => release || p.tests) } };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const event = process.env.GITHUB_EVENT_NAME;
  const ref = process.env.GITHUB_REF;
  const preparing = candidate({ event, ref, version: workspaceVersion(), changelog: readFileSync("CHANGELOG.md", "utf8"), hasTag(version) {
    try {
      execFileSync("git", ["ls-remote", "--exit-code", "--tags", "origin", `refs/tags/v${version}`], { stdio: "pipe" });
      return true;
    } catch (error) {
      if (error.status !== 2) throw error;
      return false;
    }
  } });
  const result = plan({ event, ref, rustChanged: process.env.RUST_CHANGED === "true", candidate: preparing });
  for (const [key, value] of Object.entries(result)) {
    appendFileSync(process.env.GITHUB_OUTPUT, `${key}=${JSON.stringify(value)}\n`);
  }
}
