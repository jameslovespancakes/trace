; trace-syntax query for Python (capture contract: README.md, SPEC §6.2).
; Docstrings, overloads, stubs and execution models are applied in src/python.rs.

; ---- declarations ----------------------------------------------------------------------

(function_definition
  name: (identifier) @name
  body: (block) @body) @definition.function

(class_definition
  name: (identifier) @name
  body: (block) @body) @definition.class

; Positional bases only (keyword arguments such as `metaclass=` are not bases).
(class_definition
  superclasses: (argument_list
    [(identifier) (attribute) (subscript) (call)] @base)) @definition.class

; Decorators live on the wrapper; the wrapper widens the span (spec `wrappers`).
(decorated_definition
  (decorator) @decorator
  definition: (function_definition) @definition.function)

(decorated_definition
  (decorator) @decorator
  definition: (class_definition) @definition.class)

; ---- calls ----------------------------------------------------------------------------

(call
  function: (_) @callee) @reference.call
