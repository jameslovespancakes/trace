#include <stddef.h>

typedef void (*sig_handler_t)(int);

sig_handler_t signal(int sig, sig_handler_t handler);
void qsort(void *base, size_t count, size_t size, int (*compar)(const void *, const void *));
int printf(const char *format, ...);
int atexit(void (*func)(void));
void *memcpy(void *dest, const void *src, size_t n);
