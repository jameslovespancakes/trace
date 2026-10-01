; trace-syntax query for Rust (capture contract: README.md, SPEC §6.2).
; `impl X { fn m }` -> `X.m` with container `X` (the impl block is not a symbol).
; Attributes (`#[test]`) precede items as siblings: spec `leading_attributes`.

; ---- declarations ----------------------------------------------------------------------

(function_item
  name: (identifier) @name
  body: (block) @body) @definition.function

(function_signature_item
  name: (identifier) @name) @definition.function

(impl_item
  type: (_) @container
  body: (declaration_list
    (function_item
      name: (identifier) @name
      body: (block) @body) @definition.method))

(struct_item
  name: (type_identifier) @name) @definition.class

(enum_item
  name: (type_identifier) @name
  body: (enum_variant_list) @body) @definition.class

(union_item
  name: (type_identifier) @name) @definition.class

(trait_item
  name: (type_identifier) @name
  body: (declaration_list) @body) @definition.interface

(trait_item
  bounds: (trait_bounds [(type_identifier) (scoped_type_identifier) (generic_type)] @base)) @definition.interface

; ---- out-of-line relations ----------------------------------------------------------------

(impl_item
  trait: (_) @impl.trait
  type: (_) @impl.type)

; ---- calls ----------------------------------------------------------------------------

(call_expression
  function: (_) @callee) @reference.call
