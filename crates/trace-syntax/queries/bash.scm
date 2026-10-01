; trace-syntax query for Bash (capture contract: README.md, SPEC §6.2).
; Every command is a call site; its callee is the command name.

(function_definition
  name: (word) @name
  body: (_) @body) @definition.function

(command
  name: (command_name) @callee) @reference.call
