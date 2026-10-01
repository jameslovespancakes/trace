/* Fixture (P5, c_cpp): C declarations implemented in C++. */
int cpp_impl(int x);   /* unique extern "C" definition -> proven */
int twice(int x);      /* two extern "C" definitions -> possible */
int mangled_only(int x); /* negative: the C++ definition has C++ linkage */
