# rule-ts-assigned-function (trace-semantic fixture; never installed, built or run)

Rule "assigned function" of the TypeScript worker (`assets/ts-worker/assigned.mjs`): a call
whose signature comes from a type reaches the repository function its callee holds when the
holder's initializer and every write in the repository assign that one function; rule
"returned function": a call returns the one repository function its callee returns.

| call in `src/main.ts` | expected |
|---|---|
| `app.get('/a')`, `app.post('/b')` | the arrow `this[method] = ...` assigned in `App`'s constructor (key typed `'get' \| 'post'`) |
| `app.use('/c')` | the arrow `this.use = ...` of `App`'s constructor |
| `patched.run('x')` | `first` (`this.run = first`) |
| `later('x')` | the arrow assigned to the uninitialised `let later` |
| `alias('x')` | `first` (`const alias: Handler = first as Handler`) |
| `base.hook('x')` | nothing: `Derived extends Base` declares its own `hook` |
| `twice.fn('x')` | nothing: `first` and `second` are both assigned |
| `replaced('x')` | nothing: the placeholder arrow is replaced by a call result |
| `external.exec('x')` | nothing: `src/poke.ts` writes `exec` through an `any` object |
| `made('x')` (`const made = makeHandler('m')`), `makeHandler('n')('y')` | the arrow `makeHandler` returns (rule "returned function") |
| `either(true)('z')` | nothing: `either` returns `first` or `second` |
| ``tag`a${1}` `` | `tag` (rule "tagged template": a call of its tag) |
| ``tagged`b` `` | the arrow `tagged` is initialised with (tagged template + assigned function) |
| `table.run('x')` | nothing: `src/poke.ts` writes `table[k]` with a non-literal key (the object literal is poisoned) |
