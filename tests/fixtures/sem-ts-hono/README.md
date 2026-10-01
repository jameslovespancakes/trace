# sem-ts-hono (trace-semantic fixture; never installed, built or run)

Mirrors the hono misses of the complex-refactor benchmark: `Context.notFound` /
`Context.redirect` are class-field arrow functions; most call sites are in anonymous callbacks
(`app.get('/x', (c) => c.notFound())`, `it('..', async () => {...})`, middleware factories
returning anonymous functions) or at module level.

| target | call sites | non-call uses |
|---|---|---|
| `src/context.ts` `Context.notFound` | 4: `hono.ts` `Hono.fetch`; `hono.test.ts` module-level `app.get` callback, module level `new Context().notFound()`, `it('c.notFound()')` callback | read `new Context().notFound` (module level) |
| `src/context.ts` `Context.redirect` | 5: `middleware/trailing-slash.ts` x2 (named and anonymous returned functions); `hono.test.ts` `app.get` callback, `it('c.redirect()')` callback x2 | write `new Context().redirect = ...` (module level) |

The integration test (`crates/trace-semantic/tests/live_analyzers.rs`) counts the expected call
sites mechanically from syntax facts (every call whose member is `notFound` / `redirect`) and
requires a proven edge to the target for each, plus no `unmapped_owner` diagnostic. Before
engine 4 the worker dropped every result owned by an anonymous function (`unmapped_owner`) and
never visited module-level code.
