import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { platforms, workspaceVersion } from "./ci-plan.mjs";
export { workspaceVersion } from "./ci-plan.mjs";

export function layout(target, version) {
  const platform = platforms.find(p => p.target === target);
  if (!platform) throw new Error(`Unknown release target: ${target}`);
  return {
    binary: target.includes("windows") ? "trace.exe" : "trace",
    archive: `trace-v${version}-${target}.${platform.archive}`,
  };
}

export const digest = path => createHash("sha256").update(readFileSync(path)).digest("hex");

export function recordBuild(root, { commit, run, version, target }) {
  const { binary, archive } = layout(target, version);
  const files = [`bin/${binary}`, archive];
  const manifest = { schema: 1, commit, run: String(run), version, target, files: Object.fromEntries(files.map(file => [file, digest(join(root, file))])) };
  writeFileSync(join(root, "build.json"), JSON.stringify(manifest, null, 2) + "\n");
  return manifest;
}

export function stageArtifacts(input, output, { commit, run, version }) {
  // Validate all six before writing any publishable package inputs.
  const sources = platforms.map(({ target }) => {
    const root = join(input, `release-${target}`);
    const manifest = JSON.parse(readFileSync(join(root, "build.json"), "utf8"));
    const { binary, archive } = layout(target, version);
    if (manifest.schema !== 1 || manifest.commit !== commit || manifest.run !== String(run) || manifest.version !== version || manifest.target !== target) {
      throw new Error(`Artifact identity mismatch for ${target}.`);
    }
    const files = [`bin/${binary}`, archive];
    if (Object.keys(manifest.files ?? {}).sort().join("\n") !== files.sort().join("\n")) throw new Error(`Unexpected artifact files for ${target}.`);
    for (const file of files) {
      if (digest(join(root, file)) !== manifest.files[file]) throw new Error(`Artifact checksum mismatch: ${target}/${file}`);
    }
    return { root, target, binary, archive };
  });
  mkdirSync(join(output, "dist"), { recursive: true });
  for (const { root, target, binary, archive } of sources) {
    const bin = join(output, "npm-bin", `bin-${target}`);
    mkdirSync(bin, { recursive: true });
    copyFileSync(join(root, "bin", binary), join(bin, binary));
    copyFileSync(join(root, archive), join(output, "dist", archive));
  }
  const sums = sources.map(({ archive }) => `${digest(join(output, "dist", archive))}  ${archive}`).sort();
  writeFileSync(join(output, "dist", "SHA256SUMS"), sums.join("\n") + "\n");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const identity = { commit: process.env.GITHUB_SHA, run: process.env.SOURCE_RUN ?? process.env.GITHUB_RUN_ID, version: workspaceVersion(), target: process.env.TARGET };
    if (process.argv[2] === "version") console.log(identity.version);
    else if (process.argv[2] === "record") recordBuild("release-artifact", identity);
    else if (process.argv[2] === "stage") stageArtifacts("release-assets", resolve("."), identity);
    else throw new Error("Usage: release-artifacts.mjs version|record|stage");
  } catch (error) {
    console.error(`Error: ${error.message}`);
    process.exitCode = 1;
  }
}
