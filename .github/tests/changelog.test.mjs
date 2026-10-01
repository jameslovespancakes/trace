import assert from "node:assert/strict";
import { test } from "node:test";
import { releaseNotes, validate } from "../scripts/changelog.mjs";

const text = "# Changelog\n\n## Unreleased\n\n- Next change.\n\n## 0.1.0 - 2026-10-01\n\n### Added\n- Initial release.\n\n## 0.1.0-beta.1 - 2026-09-30\n\n- Preview.\n";

test("extracts only the requested release, not Unreleased or older notes", () => {
  assert.equal(releaseNotes(text, "0.1.0"), "### Added\n- Initial release.");
  assert.equal(releaseNotes(text, "0.1.0-beta.1"), "- Preview.");
  assert.equal(releaseNotes(text.replaceAll("\n", "\r\n"), "0.1.0"), releaseNotes(text, "0.1.0"));
});

test("allows an empty Unreleased section", () => {
  assert.equal(validate(text.replace("- Next change.", "")).get("Unreleased"), "");
});

test("rejects missing releases and invalid version arguments", () => {
  assert.throws(() => releaseNotes(text, "0.2.0"), /before tagging/);
  for (const version of ["Unreleased", "v0.1.0", "", "0.1"]) {
    assert.throws(() => releaseNotes(text, version), /Invalid release version/);
  }
});

test("rejects em dashes anywhere in the changelog", () => {
  assert.throws(() => validate(text + "\nUse this\u2014not that.\n"), /em dashes/);
});

test("rejects missing, misplaced, duplicated and malformed headings", () => {
  for (const bad of [
    text.replace("## Unreleased", "## Pending"),
    text.replace("## Unreleased", "## Unreleased - 2026-10-01"),
    text + "\n## Unreleased\n",
    text + "\n## 0.1.0 - 2026-10-02\n- Duplicate.\n",
    text.replace("## 0.1.0 -", "## invalid -"),
    text.replace("## 0.1.0 - 2026-10-01", "## 0.1.0"),
  ]) assert.throws(() => validate(bad));
});

test("rejects invalid dates and empty release notes", () => {
  for (const date of ["2026-02-30", "2026-13-01", "26-10-01", "2026-1-1", "2026-10-01 - extra"]) {
    assert.throws(() => validate(text.replace("2026-10-01", date)));
  }
  assert.throws(() => validate(text.replace("- Initial release.", "")), /Add release notes/);
});
