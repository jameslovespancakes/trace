; trace-syntax query for Haskell (capture contract: README.md, SPEC §6.2).
; Function equations are declarations (each equation of a multi-clause function is one);
; their non-header children are the body (src/extract/ header mode).

(function
  name: (variable) @name) @definition.function

; Point-free / zero-argument definitions (`matches = flip match`): in Haskell a binding is a
; function value like any equation, so it is a declaration too.
(bind
  name: (variable) @name) @definition.function

; Type signatures are declarations that are not definitions (`@stub`, SPEC §6.3): one
; declaration per name, `a, b :: T` included (the name itself is the declaration node).
(signature
  name: (variable) @name) @definition.function @stub

(signature
  names: (binding_list
    name: (variable) @name @definition.function @stub))

(data_type
  name: (name) @name) @definition.class

(newtype
  name: (name) @name) @definition.class

(class
  name: (name) @name) @definition.interface

; `instance C T where ...`: an out-of-line nominal relation (type `T` implements class `C`);
; the instance's bindings are declared for the instance type (container `T`).
(instance
  name: (name) @impl.trait
  patterns: (type_patterns . (name) @impl.type))

(instance
  name: (name) @impl.trait
  patterns: (type_patterns . (parens type: (apply constructor: (name) @impl.type))))

(instance
  patterns: (type_patterns . (name) @container)
  declarations: (instance_declarations
    [(function) (bind)] @definition.function))

(instance
  patterns: (type_patterns . (parens type: (apply constructor: (name) @container)))
  declarations: (instance_declarations
    [(function) (bind)] @definition.function))

(apply
  function: (_) @callee) @reference.call

; Infix application of a named function: s `matches` re
(infix
  operator: (infix_id (variable) @callee)) @reference.call
