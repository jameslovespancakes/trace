// Fixture (P5, cgo): Go calling C through cgo.
package main

// #include <stdlib.h>
// #include "../c/lib.h"
// static int inline_add(int a, int b) { return a + b; }
import "C"

func compute() int {
	a := C.inline_add(1, 2) // unique: defined in the preamble -> proven
	b := C.lib_mul(3, 4)    // unique: defined in c/lib.c -> proven
	c := C.dup(5)           // ambiguous: defined in c/a.c and c/b.c -> possible
	s := C.CString("x")     // cgo pseudo-function: no bridge
	defer C.free(nil)
	_ = C.not_defined(6) // negative control: declared nowhere -> no bridge
	_ = s
	return int(a + b + c)
}

func main() {
	_ = compute()
}
