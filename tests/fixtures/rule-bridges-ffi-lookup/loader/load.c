/* Fixture (bridges, ffi): a symbol looked up at run time through the dynamic loader. */
#include <dlfcn.h>

typedef int (*compress_fn)(int);

int run(void *handle) {
    compress_fn f = (compress_fn)dlsym(handle, "compress_buf");
    compress_fn g = (compress_fn)dlsym(handle, "not_there"); /* negative control */
    return f(10) + (g ? 1 : 0);
}
