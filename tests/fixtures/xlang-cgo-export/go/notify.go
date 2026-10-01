// Fixture (round 3, cgo //export): C code calling Go functions that cgo exports.
package main

/*
#include <stdlib.h>
extern int wait_for_unlock(void *db);
*/
import "C"

//export wait_for_unlock
func waitForUnlock(db unsafe.Pointer) C.int { // exported to C as wait_for_unlock -> proven
	return 0
}

// A plain comment mentioning export does not export anything.
// export not_exported
func notExported() int {
	return 1
}

//export dup_symbol
func dupSymbol() C.int { // also defined in C (c/dup.c): the C prototype has two candidates -> possible
	return 2
}

func main() {
	_ = notExported()
}
