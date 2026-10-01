; trace-syntax query for TypeScript and TSX (capture contract: README.md, SPEC §6.2).
; Bodiless signatures (interface methods, overload signatures, abstract methods) are stubs;
; overload signatures stay declarations of their own (the family rule links them).

; ---- declarations ----------------------------------------------------------------------

(function_declaration
  name: (identifier) @name
  body: (statement_block) @body) @definition.function

(generator_function_declaration
  name: (identifier) @name
  body: (statement_block) @body) @definition.function

(function_signature
  name: (identifier) @name) @definition.function

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

(public_field_definition
  name: [(property_identifier) (private_property_identifier)] @name
  value: [
    (arrow_function body: (_) @body)
    (function_expression body: (statement_block) @body)
  ]) @definition.method

(method_definition
  name: [(property_identifier) (private_property_identifier)] @name
  body: (statement_block) @body) @definition.method

(method_signature
  name: [(property_identifier) (private_property_identifier)] @name) @definition.method

(abstract_method_signature
  name: [(property_identifier) (private_property_identifier)] @name) @definition.method

(class_declaration
  name: (type_identifier) @name
  body: (class_body) @body) @definition.class

(abstract_class_declaration
  name: (type_identifier) @name
  body: (class_body) @body) @definition.class

(class_declaration
  decorator: (decorator) @decorator) @definition.class

(abstract_class_declaration
  decorator: (decorator) @decorator) @definition.class

(class_declaration
  (class_heritage (extends_clause value: (_) @base))) @definition.class

(class_declaration
  (class_heritage (implements_clause (_) @base))) @definition.class

(abstract_class_declaration
  (class_heritage (extends_clause value: (_) @base))) @definition.class

(abstract_class_declaration
  (class_heritage (implements_clause (_) @base))) @definition.class

(interface_declaration
  name: (type_identifier) @name
  body: (interface_body) @body) @definition.interface

(interface_declaration
  (extends_type_clause type: (_) @base)) @definition.interface

; ---- calls ----------------------------------------------------------------------------

(call_expression
  function: (_) @callee) @reference.call

(new_expression
  constructor: (_) @callee) @reference.new

; ---- test blocks: it("name", fn) / test("name", fn) -----------------------------------

; Candidate shape only; framework imports/aliases are checked against convention data.
(call_expression
   arguments: (arguments . [(string) (template_string)] @test.name
               [(arrow_function) (function_expression)])) @test.block
