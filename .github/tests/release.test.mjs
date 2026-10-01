import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { candidate, plan, platforms } from "../scripts/ci-plan.mjs";
import { layout, recordBuild, stageArtifacts, workspaceVersion } from "../scripts/release-artifacts.mjs";
import { releaseIdentity, requireArtifacts, requireCurrent, selectRun } from "../scripts/release-source.mjs";

const require = createRequire(import.meta.url);
const { TARGETS } = require("../npm/trace/lib/platform.js");
const commit = "a".repeat(40);
const identity = { commit, run: "42", version: "0.1.0" };
const successful = { id: 42, head_sha: commit, head_branch: "main", event: "push", run_number: 1, status: "completed", conclusion: "success" };

test("normal changes test three platforms; release candidates prepare six", () => {
  const pr = plan({ event: "pull_request", ref: "refs/pull/1/merge", rustChanged: true });
  assert.equal(pr.release, false);
  assert.equal(pr.matrix.include.length, 3);
  assert.ok(pr.matrix.include.every(p => p.tests));
  assert.equal(plan({ event: "push", ref: "refs/heads/main", rustChanged: true }).matrix.include.length, 3);
  const main = plan({ event: "push", ref: "refs/heads/main", rustChanged: true, candidate: true });
  assert.equal(main.release, true);
  assert.equal(main.matrix.include.length, 6);
  assert.equal(main.matrix.include.filter(p => p.tests).length, 3);
  assert.equal(main.matrix.include.filter(p => p.lint).length, 1);
  assert.deepEqual(platforms.map(p => p.target).sort(), Object.values(TARGETS).sort());
});

test("only dated, untagged versions on main become automatic candidates", () => {
  const input = { event: "push", ref: "refs/heads/main", version: "0.1.0", changelog: "## Unreleased\n\n## 0.1.0 - 2026-10-01\n- Initial release.\n", hasTag: () => false };
  assert.equal(candidate(input), true);
  assert.equal(candidate({ ...input, hasTag: () => true }), false);
  assert.equal(candidate({ ...input, version: "0.2.0", hasTag: () => assert.fail("unneeded tag lookup") }), false);
  assert.equal(candidate({ ...input, event: "pull_request", hasTag: () => assert.fail("PR must not prepare a release") }), false);
  assert.equal(candidate({ ...input, ref: "refs/heads/feature" }), false);
  const notesOnly = plan({ event: "push", ref: "refs/heads/main", rustChanged: false, candidate: true });
  assert.equal(notesOnly.rust, true);
  assert.equal(notesOnly.release, true);
});

test("docs/npm changes skip Rust; manual main runs prepare artifacts", () => {
  const docs = plan({ event: "push", ref: "refs/heads/main", rustChanged: false });
  assert.equal(docs.rust, false);
  assert.equal(docs.release, false);
  assert.equal(plan({ event: "workflow_dispatch", ref: "refs/heads/main", rustChanged: false }).release, true);
  assert.equal(plan({ event: "workflow_dispatch", ref: "refs/heads/feature", rustChanged: false }).release, false);
});

test("requires current main and the latest successful CI on the exact commit", () => {
  requireCurrent(commit, commit);
  assert.throws(() => requireCurrent(commit, "b".repeat(40)), /behind main/);
  assert.equal(selectRun([successful], commit).id, 42);
  for (const changed of [
    { head_sha: "b".repeat(40) }, { head_branch: "feature" }, { event: "pull_request" },
    { status: "in_progress" }, { conclusion: "failure" }, { conclusion: "cancelled" },
  ]) assert.throws(() => selectRun([{ ...successful, ...changed }], commit));
  assert.throws(() => selectRun([successful, { ...successful, run_number: 2, conclusion: "failure" }], commit));
});

test("requires six unexpired artifact names and never publishes a manual tag run", () => {
  const artifacts = platforms.map(p => ({ name: `release-${p.target}`, expired: false }));
  requireArtifacts(artifacts);
  assert.throws(() => requireArtifacts(artifacts.slice(1)), /Missing or expired/);
  assert.throws(() => requireArtifacts([{ ...artifacts[0], expired: true }, ...artifacts.slice(1)]));
  assert.throws(() => requireArtifacts([...artifacts, artifacts[0]]));
  const tag = { event: "push", refType: "tag", refName: "v0.1.0", version: "0.1.0" };
  assert.equal(releaseIdentity(tag).publish, true);
  assert.equal(releaseIdentity({ ...tag, event: "workflow_dispatch" }).publish, false);
  assert.throws(() => releaseIdentity({ ...tag, refName: "v0.2.0" }), /Tag must match/);
  assert.equal(releaseIdentity({ ...tag, refName: "v0.1.0-beta.1", version: "0.1.0-beta.1" }).prerelease, true);
});

test("reads only the workspace version and rejects unknown targets", () => {
  assert.equal(workspaceVersion('[workspace.package]\nversion = "0.1.0"\n[dependencies]\nversion = "9.0.0"\n'), "0.1.0");
  assert.throws(() => workspaceVersion('[package]\nversion = "0.1.0"\n'));
  assert.throws(() => layout("unknown", "0.1.0"));
});

function fixture(fn) {
  const root = mkdtempSync(join(tmpdir(), "trace-release-"));
  const input = join(root, "input");
  try {
    for (const { target } of platforms) {
      const dir = join(input, `release-${target}`);
      const { binary, archive } = layout(target, identity.version);
      mkdirSync(join(dir, "bin"), { recursive: true });
      writeFileSync(join(dir, "bin", binary), `test binary ${target}`);
      writeFileSync(join(dir, archive), `test archive ${target}`);
      recordBuild(dir, { ...identity, target });
    }
    fn({ root, input, output: join(root, "output") });
  } finally { rmSync(root, { recursive: true, force: true }); }
}

test("stages matching artifacts for npm and GitHub with archive checksums", () => fixture(({ input, output }) => {
  stageArtifacts(input, output, identity);
  for (const { target } of platforms) {
    const { binary, archive } = layout(target, identity.version);
    assert.equal(readFileSync(join(output, "npm-bin", `bin-${target}`, binary), "utf8"), `test binary ${target}`);
    assert.equal(readFileSync(join(output, "dist", archive), "utf8"), `test archive ${target}`);
  }
  assert.equal(readFileSync(join(output, "dist", "SHA256SUMS"), "utf8").trim().split("\n").length, 6);
}));

test("rejects other commits, runs, versions, targets and tampered bytes", () => fixture(({ input, output }) => {
  const target = platforms[0].target;
  const dir = join(input, `release-${target}`);
  const manifestPath = join(dir, "build.json");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  for (const changed of [
    { commit: "b".repeat(40) }, { run: "43" }, { version: "0.2.0" }, { target: "wrong" },
    { schema: 2 }, { files: { "../../outside": "hash" } },
  ]) {
    writeFileSync(manifestPath, JSON.stringify({ ...manifest, ...changed }));
    assert.throws(() => stageArtifacts(input, output, identity));
  }
  writeFileSync(manifestPath, JSON.stringify(manifest));
  writeFileSync(join(dir, "bin", layout(target, identity.version).binary), "tampered");
  assert.throws(() => stageArtifacts(input, output, identity), /checksum mismatch/);
}));
