; trace-syntax query for C++ (capture contract: README.md, SPEC §6.2).
; Out-of-line members `void A::f()` -> `A.f` with container `A`; constructors are members
; named like their class (src/extract/).

; ---- declarations ----------------------------------------------------------------------

(function_definition
  declarator: (function_declarator
    declarator: [(identifier) (field_identifier) (destructor_name) (operator_name)] @name)
  body: (compound_statement) @body) @definition.function

(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      scope: (_) @container
      name: [(identifier) (destructor_name) (operator_name)] @name))
  body: (compound_statement) @body) @definition.method

(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (qualified_identifier
        scope: (_) @container
        name: [(identifier) (destructor_name) (operator_name)] @name)))
  body: (compound_statement) @body) @definition.method

(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: [(identifier) (field_identifier)] @name))
  body: (compound_statement) @body) @definition.function

(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (qualified_identifier
        scope: (_) @container
        name: (identifier) @name)))
  body: (compound_statement) @body) @definition.method

(function_definition
  declarator: (reference_declarator
    (function_declarator
      declarator: [(identifier) (field_identifier)] @name))
  body: (compound_statement) @body) @definition.function

(function_definition
  declarator: (reference_declarator
    (function_declarator
      declarator: (qualified_identifier
        scope: (_) @container
        name: (identifier) @name)))
  body: (compound_statement) @body) @definition.method

(declaration
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition.function

(declaration
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @definition.function

; Prototypes and forward declarations are stub declarations (no body; SPEC §6.3): returning
; references / double pointers, and qualified redeclarations (`friend void A::f();`).
(declaration
  declarator: (pointer_declarator
    declarator: (pointer_declarator
      declarator: (function_declarator
        declarator: (identifier) @name)))) @definition.function

(declaration
  declarator: (reference_declarator
    (function_declarator
      declarator: (identifier) @name))) @definition.function

(declaration
  declarator: (function_declarator
    declarator: (qualified_identifier
      scope: (_) @container
      name: [(identifier) (destructor_name) (operator_name)] @name))) @definition.method

(field_declaration
  declarator: (function_declarator
    declarator: [(field_identifier) (destructor_name) (operator_name)] @name)) @definition.method

(field_declaration
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: [(field_identifier) (operator_name)] @name))) @definition.method

(field_declaration
  declarator: (reference_declarator
    (function_declarator
      declarator: [(field_identifier) (operator_name)] @name))) @definition.method

(class_specifier
  name: (type_identifier) @name
  body: (field_declaration_list) @body) @definition.class

(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list) @body) @definition.class

(class_specifier
  (base_class_clause [(type_identifier) (qualified_identifier) (template_type)] @base)) @definition.class

(struct_specifier
  (base_class_clause [(type_identifier) (qualified_identifier) (template_type)] @base)) @definition.class

; Function-like macros are callable declarations (`#define F(x) ...`): a call `F(a)` maps to
; the `#define` name like any function (spec `macro_definitions`: never a stub).
(preproc_function_def
  name: (identifier) @name
  value: (preproc_arg)? @body) @definition.function

; ---- calls ----------------------------------------------------------------------------

(call_expression
  function: (_) @callee) @reference.call

(new_expression
  type: (_) @callee) @reference.new
