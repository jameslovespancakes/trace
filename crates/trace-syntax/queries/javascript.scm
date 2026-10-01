; trace-syntax query for JavaScript / JSX (capture contract: README.md, SPEC §6.2).
; Named function expressions and arrows bound by `const f = ...`, `obj.f = ...` and class
; fields are declarations named by the binding; anonymous callables are not declarations.

; ---- declarations ----------------------------------------------------------------------

(function_declaration
  name: (identifier) @name
  body: (statement_block) @body) @definition.function

(generator_function_declaration
  name: (identifier) @name
  body: (statement_block) @body) @definition.function

(function_expression
  name: (identifier) @name
  body: (statement_block) @body) @definition.function

(generator_function
  name: (identifier) @name
  body: (statement_block) @body) @definition.function

(variable_declarator
  name: (identifier) @name
  value: [
    (arrow_function body: (_) @body)
    (function_expression body: (statement_block) @body)
    (generator_function body: (statement_block) @body)
  ]) @definition.function

; `obj.prop = function ...` / `Foo.prototype.m = ...`: the receiver path is the container
; (src/extract/ `property_container`: `this`, `exports`, `module.exports` name no container;
; `X.prototype` names `X`), so the selector `obj.prop` matches the declaration exactly.
(assignment_expression
  left: [
    (identifier) @name
    (member_expression
      object: (_) @container
      property: (property_identifier) @name)
  ]
  right: [
    (arrow_function body: (_) @body)
    (function_expression body: (statement_block) @body)
    (generator_function body: (statement_block) @body)
  ]) @definition.function

(field_definition
  property: [(property_identifier) (private_property_identifier)] @name
  value: [
    (arrow_function body: (_) @body)
    (function_expression body: (statement_block) @body)
  ]) @definition.method

(method_definition
  name: [(property_identifier) (private_property_identifier)] @name
  body: (statement_block) @body) @definition.method

(method_definition
  decorator: (decorator) @decorator) @definition.method

(class_declaration
  name: (identifier) @name
  body: (class_body) @body) @definition.class

(class_declaration
  decorator: (decorator) @decorator) @definition.class

(class_declaration
  (class_heritage (_) @base)) @definition.class

; ---- calls ----------------------------------------------------------------------------

(call_expression
  function: (_) @callee) @reference.call

(new_expression
  constructor: (_) @callee) @reference.new

; ---- test blocks: it("name", fn) / test("name", fn) -----------------------------------

; Candidate shape only. Framework/import identities and aliases are checked in Rust
; from the language convention data; no project paths or test-title lists are used.
(call_expression
   arguments: (arguments . [(string) (template_string)] @test.name
               [(arrow_function) (function_expression)])) @test.block
