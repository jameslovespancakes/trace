# Changelog

Notable changes, newest first.

## Unreleased

## 0.1.0 - 2026-10-01

### Added
- Read-only code navigation across 14 languages.
- Complete numbered source, symbol discovery, literal search and source excerpts.
- Caller, dependency and call-path analysis.
- One npm install command that selects the Windows, Linux or macOS binary for x64 or ARM64.
- Changelog checks and curated release notes, included in the launcher package and archives.

### Build and release
- Native checks run in parallel with a faster test profile and retained workspace caches.
- Only release candidates build all six shipping targets. Ordinary documentation and npm-only changes skip Rust builds.
- Publishing reuses checksum-verified CI artifacts without rerunning tests or compiling on tag pushes.
- Releases require successful CI for the exact current main commit. Manual Release runs never publish.
