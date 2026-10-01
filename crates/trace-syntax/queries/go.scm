; trace-syntax query for Go (capture contract: README.md, SPEC §6.2).
; `func (s *Server) Run()` -> `Server.Run` with container `Server`.

; ---- declarations ----------------------------------------------------------------------

(function_declaration
  name: (identifier) @name
  body: (block)? @body) @definition.function

(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: [
        (type_identifier) @container
        (pointer_type (type_identifier) @container)
        (generic_type type: (type_identifier) @container)
        (pointer_type (generic_type type: (type_identifier) @container))
      ]))
  name: (field_identifier) @name
  body: (block)? @body) @definition.method

(type_spec
  name: (type_identifier) @name
  type: (struct_type)) @definition.class

(type_spec
  name: (type_identifier) @name
  type: (interface_type) @body) @definition.interface

(type_spec
  type: (interface_type
    (method_elem
      name: (field_identifier) @name) @definition.method))

; ---- calls ----------------------------------------------------------------------------

(call_expression
  function: (_) @callee) @reference.call
