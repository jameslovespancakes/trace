# trace-syntax queries

One tags-style query per language: `<language>.scm` (`python.scm`, `javascript.scm`,
`typescript.scm`, `tsx.scm`, `rust.scm`, `go.scm`, `java.scm`, `c.scm`, `cpp.scm`,
`csharp.scm`, `php.scm`, `bash.scm`, `scala.scm`, `r.scm`, `haskell.scm`). Embedded with
`include_str!`.

Upstream grammar crates ship `queries/tags.scm` for most languages; these queries started
from those (MIT-licensed) and extend them to the capture contract below. See SPEC.md §6.2.

| capture                    | on node                     | meaning                                              |
|----------------------------|-----------------------------|------------------------------------------------------|
| `@definition.function`     | whole declaration           | free function / named function expression            |
| `@definition.method`       | whole declaration           | method (callable inside a class-like body)           |
| `@definition.constructor`  | whole declaration           | language-level constructor                           |
| `@definition.class`        | whole declaration           | class / struct / enum / record / object              |
| `@definition.interface`    | whole declaration           | interface / trait / protocol                         |
| `@name`                    | identifier                  | declared name (same match as a `@definition.*`)      |
| `@body`                    | body node                   | body (sets `body_start`; absent => stub/prototype)   |
| `@decorator`               | decorator/attribute node    | outermost-first; widens the declaration span         |
| `@base`                    | base type expression        | superclass / implemented interface spelling          |
| `@parameter`               | parameter name identifier   | positional parameter (in order)                      |
| `@parameter.keyword`       | parameter name identifier   | keyword-only parameter                               |
| `@parameter.variadic`      | parameter name identifier   | `*args` / `...rest` / `**kwargs`                     |
| `@parameter.default`       | default value expression    | default of the preceding parameter                   |
| `@doc`                     | string / comment node       | docstring or doc comment of the declaration          |
| `@reference.call`          | whole call expression       | call site                                            |
| `@callee`                  | callee expression           | function part of the call (same match)               |
| `@reference.new`           | whole `new` expression      | constructor call                                     |
| `@impl.type` / `@impl.trait` | type identifiers          | out-of-line `impl Trait for Type` relation           |
| `@container`               | receiver type / path        | Go receiver / Rust impl self type, Haskell instance type, receiver path of JS/TS property-assigned functions |
| `@stub`                    | the definition node         | declaration that is not a definition (`is_stub`): Haskell type signatures |
| `@test.name`               | string literal              | JS/TS `it("name", ...)` / `test("name", ...)`        |
| `@test.block`              | whole call                  | the test block span                                  |

Several patterns may capture the same definition node; their captures are merged (the most
specific definition kind wins: constructor > method > function, interface > class). Two
definitions with the same `@body` (a named function expression bound by `const f = ...`)
are one declaration: the outermost node wins.

Anonymous callables (`SyntaxSpec::anonymous_functions`, extractor 5) are synthetic
`<lambda>` declarations, except when their body is the `@body` of a definition (`const f =
() => {}`, `obj.f = function () {}`, class fields, R `f <- function(x) ...`): such a function value is that named
definition. Queries therefore capture the binding (with the function's body as `@body`),
never the anonymous node itself.

Parts that are *structure*, not captures, come from `src/languages/<lang>.rs` (the language's `SyntaxSpec`, fields documented in `src/spec.rs`):

* parameters: when a definition match has no `@parameter*` captures (all shipped queries),
  parameters are read with the language's `params` rules from the callable's parameter
  list (names, variadic/keyword-only kinds, default values);
* span widening: `wrappers` (`export`, `decorated_definition`, `template<...>`, a single
  `const f = ...`) and `leading_attributes` (Rust `#[...]`, TS member decorators);
* ownership (`@body` subtree executes when called; lazy scopes; generator expressions);
* receivers, members, arguments, assignments, returns and loops for value-flow lowering.

Captures whose names start with `_` (e.g. `@_test_fn`, `@_keyword`) only feed predicates
and are otherwise ignored.

Rules: no `#match?`/`#eq?` predicates on source text for extraction decisions except
node-kind/field structure (predicates are allowed only to pin literal callee names such as
`it`/`test`/`describe`, which is
structural, not source regex extraction).

Every query is validated at startup: a query that fails to compile makes its language
unavailable (reported by `trace_syntax::grammar_errors()` and `trace status`), never a panic.
