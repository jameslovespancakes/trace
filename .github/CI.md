# CI and releases

## Checks

[CI](workflows/ci.yml) runs formatting, Clippy and npm launcher checks, plus Rust tests on Windows, Linux and macOS. Unchanged areas are skipped. Require **Result** for branch protection.

Local checks:

```sh
cargo fmt --all -- --check
cargo clippy --release --workspace --all-targets --locked -- -D warnings
cargo nextest run --release --workspace --locked --profile ci
node --test .github/npm/test/*.test.mjs
```

CI disables automatic language-server installation. Tests needing an unavailable server skip.

## Releases

1. Update the workspace version in `Cargo.toml`, run `cargo check` to update `Cargo.lock`, and commit.
2. Push a matching `vMAJOR.MINOR.PATCH` tag, optionally with a prerelease suffix.

[Release](workflows/release.yml) gates on CI, builds x64 and ARM64 binaries for all three platforms, and publishes npm packages plus GitHub archives and checksums.

- npm publishing requires `NPM_TOKEN`. Signing is optional; configuration is in the workflow.
- Prereleases use `next`, plus `latest` until a stable release exists.
- A manual workflow run builds and packs without publishing.

## Live tests

[Live servers](workflows/live.yml) runs nightly and on demand. It installs default servers into an empty tools folder and tests them on all three platforms. Results are informational, not a release gate.
