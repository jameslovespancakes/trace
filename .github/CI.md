# CI and releases

## Checks

[CI](workflows/ci.yml) runs lightweight tooling and changelog checks on every change. Require **Result** for branch protection.

- Normal Rust changes test Windows, Linux and macOS in parallel. Formatting and Clippy run once on Linux.
- A main commit becomes a release candidate when its workspace version has a dated changelog entry but no version tag. Only candidates build all six shipping targets, once each.
- Tests use the `ci` Cargo profile without release LTO. Shipping binaries keep the optimized release profile.
- Ordinary documentation and npm-only changes skip Rust builds. Preparing a release or running CI manually on main enables the full candidate matrix.
- Workspace build caches are retained. Release artifacts expire after seven days.

Every pull request and main push must update `CHANGELOG.md`. Add notes under `Unreleased`; em dashes are rejected.

Local checks:

```sh
cargo fmt --all -- --check
cargo clippy --profile ci --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked --cargo-profile ci --profile ci
node --test .github/tests/*.test.mjs .github/npm/test/*.test.mjs
node .github/scripts/changelog.mjs
```

CI disables automatic language-server installation. Tests needing an unavailable server skip.

## Releases

1. Move `Unreleased` notes into a dated version entry. Set the same version in `Cargo.toml` and update `Cargo.lock`.
2. Create and push a new commit. Wait for its CI to pass with all six release artifacts. Run CI manually on main if artifacts are missing or expired.
3. Tag that current main commit with the matching `vMAJOR.MINOR.PATCH`, optionally with a prerelease suffix, and push the tag.

[Release](workflows/release.yml) verifies the commit, CI result, version and artifact checksums. It downloads those binaries without rerunning CI or compiling again, then publishes npm packages and GitHub archives. A final main check blocks stale candidates before publication.

- Users install only `@jameslovespancakes/trace`. npm selects the matching OS/CPU package automatically.
- npm requires `NPM_TOKEN` with publish rights. Optional signing runs during CI builds; its secrets are listed in that workflow.
- Changelog entries become release notes. Prereleases use `next`, plus `latest` until a stable release exists.
- Manual Release runs reuse artifacts for a packaging dry run. They never publish, including when run on a tag.
- Do not amend a released commit or move a published tag. Changes go in a new commit and version.

## Live tests

[Live servers](workflows/live.yml) runs nightly and on demand. It installs default servers and tests all three platforms. It is informational, not a release gate.
