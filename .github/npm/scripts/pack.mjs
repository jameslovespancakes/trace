// Assemble the npm packages of one release from the release binaries.
//
//   node .github/npm/scripts/pack.mjs --version 0.9.0-beta.1 --binaries <dir> --out <dir>
//
// <binaries> holds one folder per Rust target, as the release workflow downloads them:
// `bin-<target>/trace` (`trace.exe` on Windows). Writes <out>/<platform key>/ (one package per
// platform: package.json with `os` / `cpu`, bin/<binary>, LICENSE) and <out>/trace/ (the
// launcher: bin/, lib/, package.json with the version and every platform package as an
// optional dependency, README.md, LICENSE). Every platform must have its binary.

import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const TEMPLATE = resolve(HERE, "..", "trace");
const REPO = resolve(HERE, "..", "..", "..");
const { TARGETS, packageName, binaryName } = require(join(TEMPLATE, "lib", "platform.js"));

const SEMVER = /^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/;

/** Build every package; returns `{ main, platforms: [{ key, dir, name }] }`. */
export function pack({ version, binaries, out, template = TEMPLATE, repo = REPO }) {
  if (!SEMVER.test(version)) {
    throw new Error(`Not a release version: ${version} (expected e.g. 0.9.0 or 0.9.0-beta.1).`);
  }
  const base = JSON.parse(readFileSync(join(template, "package.json"), "utf8"));
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  const license = join(repo, "LICENSE");
  const platforms = [];
  for (const [key, target] of Object.entries(TARGETS)) {
    const [platform, cpu] = key.split("-");
    const bin = binaryName(platform);
    const source = join(binaries, `bin-${target}`, bin);
    if (!existsSync(source)) {
      throw new Error(`Missing binary for ${key}: ${source}`);
    }
    const dir = join(out, key);
    mkdirSync(join(dir, "bin"), { recursive: true });
    copyFileSync(source, join(dir, "bin", bin));
    chmodSync(join(dir, "bin", bin), 0o755);
    if (existsSync(license)) copyFileSync(license, join(dir, "LICENSE"));
    const name = packageName(base.name, key);
    const manifest = {
      name,
      version,
      description: `The ${platform} ${cpu} binary of ${base.name} (installed automatically; install ${base.name} instead).`,
      license: base.license,
      repository: base.repository,
      homepage: base.homepage,
      os: [platform],
      cpu: [cpu],
      files: ["bin/"],
      preferUnplugged: true,
    };
    writeFileSync(join(dir, "package.json"), `${JSON.stringify(manifest, null, 2)}\n`);
    platforms.push({ key, dir, name });
  }
  const main = join(out, "trace");
  mkdirSync(main, { recursive: true });
  cpSync(join(template, "bin"), join(main, "bin"), { recursive: true });
  cpSync(join(template, "lib"), join(main, "lib"), { recursive: true });
  chmodSync(join(main, "bin", "trace.js"), 0o755);
  if (existsSync(license)) copyFileSync(license, join(main, "LICENSE"));
  const readme = join(repo, "README.md");
  if (existsSync(readme)) copyFileSync(readme, join(main, "README.md"));
  const manifest = {
    ...base,
    version,
    optionalDependencies: Object.fromEntries(platforms.map((p) => [p.name, version])),
  };
  writeFileSync(join(main, "package.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  return { main: { dir: main, name: base.name }, platforms };
}

function args(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 2) {
    const flag = argv[i];
    if (!flag.startsWith("--") || argv[i + 1] === undefined) {
      throw new Error(`Usage: node pack.mjs --version <v> --binaries <dir> --out <dir>`);
    }
    out[flag.slice(2)] = argv[i + 1];
  }
  for (const need of ["version", "binaries", "out"]) {
    if (!out[need]) throw new Error(`Missing --${need}`);
  }
  return out;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  try {
    const a = args(process.argv.slice(2));
    const packed = pack({ version: a.version, binaries: resolve(a.binaries), out: resolve(a.out) });
    for (const p of packed.platforms) console.log(`${p.name}@${a.version}  ${p.dir}`);
    console.log(`${packed.main.name}@${a.version}  ${packed.main.dir}`);
  } catch (err) {
    console.error(`Error: ${err.message}`);
    process.exit(1);
  }
}
