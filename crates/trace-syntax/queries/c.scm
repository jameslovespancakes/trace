; trace-syntax query for C (capture contract: README.md, SPEC §6.2).
; Prototypes (declarations with a function declarator) are stub declarations.

; ---- declarations ----------------------------------------------------------------------

(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)
  body: (compound_statement) @body) @definition.function

(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))
  body: (compound_statement) @body) @definition.function

(function_definition
  declarator: (pointer_declarator
    declarator: (pointer_declarator
      declarator: (function_declarator
        declarator: (identifier) @name)))
  body: (compound_statement) @body) @definition.function

(declaration
  declarator: (function_declarator
    declarator: (identifier) @name)) @definition.function

(declaration
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @name))) @definition.function

; `char **names(void);`
(declaration
  declarator: (pointer_declarator
    declarator: (pointer_declarator
      declarator: (function_declarator
        declarator: (identifier) @name)))) @definition.function

(struct_specifier
  name: (type_identifier) @name
  body: (field_declaration_list) @body) @definition.class

(union_specifier
  name: (type_identifier) @name
  body: (field_declaration_list) @body) @definition.class

; Function-like macros are callable declarations (`#define F(x) ...`): a call `F(a)` maps to
; the `#define` name like any function (spec `macro_definitions`: never a stub).
(preproc_function_def
  name: (identifier) @name
  value: (preproc_arg)? @body) @definition.function

; ---- calls ----------------------------------------------------------------------------

(call_expression
  function: (_) @callee) @reference.call
