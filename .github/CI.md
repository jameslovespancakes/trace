# CI and releases

## Checks

[CI](workflows/ci.yml) checks the changelog, formatting, Clippy and npm launcher, plus Rust tests on Windows, Linux and macOS. Unchanged areas are skipped. Require **Result** for branch protection.

Local checks:

```sh
cargo fmt --all -- --check
cargo clippy --release --workspace --all-targets --locked -- -D warnings
cargo nextest run --release --workspace --locked --profile ci
node --test .github/tests/*.test.mjs .github/npm/test/*.test.mjs
node .github/scripts/changelog.mjs
```

Every pull request and main push must update `CHANGELOG.md`. CI checks its format and rejects em dashes. Add changes under `Unreleased`.

CI disables automatic language-server installation. Tests needing an unavailable server skip.

## Releases

1. Move `Unreleased` notes into a dated version entry in `CHANGELOG.md`.
2. Set the same version in `Cargo.toml`, run `cargo check` to update `Cargo.lock`, and commit.
3. Push the matching `vMAJOR.MINOR.PATCH` tag, optionally with a prerelease suffix.

[Release](workflows/release.yml) gates on CI, builds x64 and ARM64 binaries for all three platforms, and publishes npm packages plus GitHub archives and checksums.

- Tags require a matching changelog entry, which becomes the GitHub release notes.
- npm publishing requires `NPM_TOKEN`. Signing is optional; configuration is in the workflow.
- Prereleases use `next`, plus `latest` until a stable release exists.
- A manual workflow run builds and packs without publishing.

## Live tests

[Live servers](workflows/live.yml) runs nightly and on demand. It installs default servers into an empty tools folder and tests them on all three platforms. Results are informational, not a release gate.
