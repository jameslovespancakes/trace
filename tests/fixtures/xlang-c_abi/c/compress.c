/* Fixture (P5, c_abi): C definitions used from Rust. */
#include "api.h"

static int helper(int x) { return x * 2; }

int c_compress(int x) { return helper(x) + rs_add(x, 1); }

/* Negative control: same name as a Rust function that is not exported. */
int local_only(int x) { return x; }
