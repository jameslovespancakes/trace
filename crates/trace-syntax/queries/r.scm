; trace-syntax query for R (capture contract: README.md, SPEC §6.2).
; `f <- function(x) { ... }` declares `f`.

(binary_operator
  lhs: (identifier) @name
  operator: ["<-" "=" "<<-"]
  rhs: (function_definition
    body: (_) @body)) @definition.function

(call
  function: (_) @callee) @reference.call
