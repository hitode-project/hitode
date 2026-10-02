/* Only the CRT byte routines referenced by the unmodified upstream C sources. */
#include "patch_headers/string.h"
void *memcpy(void *dst, const void *src, size_t n) {
    volatile unsigned char *d = dst;
    const volatile unsigned char *s = src;
    while (n--) *d++ = *s++;
    return dst;
}
void *memset(void *dst, int value, size_t n) {
    volatile unsigned char *d = dst;
    while (n--) *d++ = (unsigned char)value;
    return dst;
}
