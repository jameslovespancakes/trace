apply_fun <- function(x, FUN) {
  FUN(x)
}

call_later <- function(f, args) {
  do.call(f, args)
}
