// Fixture (P5, c_cpp): C++ definitions with and without C linkage.
extern "C" {
int cpp_impl(int x) { return x + 1; }
int twice(int x) { return 2 * x; }
}

int mangled_only(int x) { return x; }
