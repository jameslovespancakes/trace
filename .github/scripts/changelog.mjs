import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const VERSION = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

export function validate(text) {
  text = text.replace(/\r\n/g, "\n");
  if (text.includes("\u2014")) throw new Error("Use plain punctuation, not em dashes, in CHANGELOG.md.");
  const headings = [...text.matchAll(/^## (.+)$/gm)];
  if (headings[0]?.[1] !== "Unreleased") throw new Error("Put ## Unreleased first in CHANGELOG.md.");
  const entries = new Map();
  for (const [i, heading] of headings.entries()) {
    const [version, date, ...extra] = heading[1].split(" - ");
    if (extra.length || (version !== "Unreleased" && !VERSION.test(version))) {
      throw new Error(`Invalid changelog heading: ${heading[1]}`);
    }
    if (entries.has(version)) throw new Error(`Duplicate changelog entry: ${version}`);
    if (version === "Unreleased") {
      if (date) throw new Error("Unreleased must not have a release date.");
    } else {
      const parsed = new Date(`${date}T00:00:00.000Z`);
      if (!/^\d{4}-\d{2}-\d{2}$/.test(date ?? "") || !Number.isFinite(parsed.getTime()) || parsed.toISOString().slice(0, 10) !== date) {
        throw new Error(`Use a valid YYYY-MM-DD release date for ${version}.`);
      }
    }
    const body = text.slice(heading.index + heading[0].length, headings[i + 1]?.index ?? text.length).trim();
    if (version !== "Unreleased" && !/^- \S/m.test(body)) throw new Error(`Add release notes for ${version}.`);
    entries.set(version, body);
  }
  return entries;
}

export function releaseNotes(text, version) {
  if (!VERSION.test(version)) throw new Error(`Invalid release version: ${version}`);
  const entries = validate(text);
  if (!entries.has(version)) throw new Error(`Add a dated ${version} entry to CHANGELOG.md before tagging.`);
  return entries.get(version);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    if (process.argv.length > 3) throw new Error("Usage: node .github/scripts/changelog.mjs [version]");
    const text = readFileSync(new URL("../../CHANGELOG.md", import.meta.url), "utf8");
    if (process.argv[2]) console.log(releaseNotes(text, process.argv[2]));
    else {
      validate(text);
      console.log("Changelog valid.");
    }
  } catch (error) {
    console.error(`Error: ${error.message}`);
    process.exitCode = 1;
  }
}
