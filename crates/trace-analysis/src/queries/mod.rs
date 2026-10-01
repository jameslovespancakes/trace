//! The query commands and the views they share.
//!
//! * [`show`]    — `show`: the exact source of symbols.
//! * [`uses`]    — `uses`: every use of a symbol ([`references`] rows) with its conditions,
//!   the entry points and tests that matter, the provenance of its links ([`evidence`]) and,
//!   with `--deep`, callers of callers and every test ([`impact`], [`similar`]).
//! * [`deps`]    — `deps`: one row per site with its condition.
//! * [`path`]    — `path`: hops with conditions and carried values.
//! * [`context`] — `context`: a symbol's source, its callers and calls, tests, next steps.
//! * [`sites`]   — query-time site facts (conditions, one-line calls, carried values,
//!   option-setting tests, entry points).
//! * [`tests_index`] — tests guarding a symbol.

pub mod audit;
pub mod context;
pub mod deps;
pub mod evidence;
pub mod impact;
pub mod path;
pub mod references;
pub mod show;
pub mod similar;
pub mod sites;
pub mod source;
pub(crate) mod tests_index;
pub mod uses;
