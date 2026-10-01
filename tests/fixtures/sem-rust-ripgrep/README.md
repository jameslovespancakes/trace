# sem-rust-ripgrep (trace-semantic fixture; never built, run or tested)

Mirrors the three ripgrep references trace lost in the complex-refactor benchmark
(`artifacts/trace-next/baseline/complex-refs`):

| target | expected call sites |
|---|---|
| `crates/printer/src/jsont.rs` `Data::from_bytes` | 4 (`&Data::from_bytes(..)` inside three `impl<'a> serde::Serialize for X<'a>` blocks) |
| `crates/ignore/src/walk.rs` `WalkBuilder::current_dir` | 1 (last step of a 20-step builder chain in `crates/core/hiargs.rs`, another crate) |
| `crates/printer/src/util.rs` `Replacer::clear` | 3 (`standard.rs` x2 in a generic impl, `json.rs` x1 behind `#[cfg(feature = "serde")]`) |

Negative control: `space.clear()` inside `Replacer::clear` is `Vec::clear` (std), not a caller.

Root causes found on ripgrep (P3 diagnosis, rust-analyzer 1.92 on a snapshot copy):
1. `cargo metadata --no-deps` (the only offline-safe cargo mode) has no dependency graph and
   no enabled features: `crates/core` could not see `ignore`, and `jsont.rs` / `json.rs` (behind
   the default `serde` feature) "did not belong to any crate" - every request there returned
   nothing. Fix: trace generates `rust-project.json` (all packages, path deps, every feature as
   cfg, sysroot) and passes it as `linkedProjects`; no cargo command runs.
2. The outer call of a builder chain has a callee span containing every step; the old
   coverage check (`any point inside the callee`) marked it covered by the first step. Fix:
   outgoing ranges are matched by the member identifier's end; uncovered calls get
   `textDocument/definition` (batch C).
