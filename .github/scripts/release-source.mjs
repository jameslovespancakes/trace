import { appendFileSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { platforms } from "./ci-plan.mjs";
import { workspaceVersion } from "./release-artifacts.mjs";
import { releaseNotes } from "./changelog.mjs";

export function requireCurrent(commit, main) {
  if (commit !== main) throw new Error("The release commit is behind main. Prepare a new commit and tag it only after its CI succeeds.");
}

export function selectRun(runs, commit) {
  const candidates = runs.filter(r => r.head_sha === commit && r.head_branch === "main" && ["push", "workflow_dispatch"].includes(r.event));
  candidates.sort((a, b) => b.run_number - a.run_number);
  const run = candidates[0];
  if (!run || run.status !== "completed" || run.conclusion !== "success") {
    throw new Error("The latest CI run for this exact main commit must succeed before release. No CI will be rerun by publishing.");
  }
  return run;
}

export function requireArtifacts(artifacts) {
  for (const { target } of platforms) {
    const matches = artifacts.filter(a => a.name === `release-${target}` && !a.expired);
    if (matches.length !== 1) throw new Error(`Missing or expired release artifact for ${target}. Run CI manually on this commit before tagging.`);
  }
}

export function releaseIdentity({ event, refType, refName, version }) {
  if (refType === "tag" && refName !== `v${version}`) throw new Error("Tag must match the workspace version.");
  return { version, publish: event === "push" && refType === "tag", prerelease: version.includes("-") };
}

async function api(path) {
  const response = await fetch(`https://api.github.com/repos/${process.env.GITHUB_REPOSITORY}/${path}`, {
    headers: { Authorization: `Bearer ${process.env.GH_TOKEN}`, Accept: "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28" },
  });
  if (!response.ok) throw new Error(`GitHub API ${response.status} for ${path}`);
  return response.json();
}

async function main() {
  const commit = process.env.GITHUB_SHA;
  requireCurrent(commit, (await api("commits/main")).sha);
  if (process.argv[2] === "current") {
    const run = await api(`actions/runs/${process.env.SOURCE_RUN}`);
    selectRun([run], commit);
    return;
  }
  if (process.argv[2] !== "verify") throw new Error("Usage: release-source.mjs verify|current");
  const version = workspaceVersion();
  releaseNotes(readFileSync("CHANGELOG.md", "utf8"), version);
  const identity = releaseIdentity({ event: process.env.GITHUB_EVENT_NAME, refType: process.env.GITHUB_REF_TYPE, refName: process.env.GITHUB_REF_NAME, version });
  const runs = await api(`actions/workflows/ci.yml/runs?head_sha=${commit}&branch=main&per_page=20`);
  const run = selectRun(runs.workflow_runs, commit);
  requireArtifacts((await api(`actions/runs/${run.id}/artifacts?per_page=100`)).artifacts);
  for (const [key, value] of Object.entries({ ...identity, run: run.id })) {
    appendFileSync(process.env.GITHUB_OUTPUT, `${key}=${value}\n`);
  }
  console.log(`Reusing CI run ${run.id} for ${commit}. No rebuilds.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => { console.error(`Error: ${error.message}`); process.exitCode = 1; });
}
