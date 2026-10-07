// Owned support for the upstream static library under an explicit Win32 entry
// point. Upstream sources and allocation/import-editing logic remain unchanged.
// Allocation uses the process heap; the fixture does not initialize a static CRT.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stdarg.h>

void *operator new(unsigned int size) { return HeapAlloc(GetProcessHeap(), 0, size ? size : 1); }
void *operator new[](unsigned int size) { return HeapAlloc(GetProcessHeap(), 0, size ? size : 1); }
void operator delete(void *pointer) { if (pointer) HeapFree(GetProcessHeap(), 0, pointer); }
void operator delete[](void *pointer) { if (pointer) HeapFree(GetProcessHeap(), 0, pointer); }
extern "C" void *memcpy(void *destination, const void *source, unsigned int size) {
    volatile BYTE *out = (volatile BYTE *)destination;
    const volatile BYTE *in = (const volatile BYTE *)source;
    for (unsigned int index = 0; index < size; index++) out[index] = in[index];
    return destination;
}
extern "C" void *memset(void *pointer, int value, unsigned int size) {
    volatile BYTE *bytes = (volatile BYTE *)pointer;
    for (unsigned int index = 0; index < size; index++) bytes[index] = (BYTE)value;
    return pointer;
}
extern "C" unsigned int strlen(const char *value) {
    unsigned int result = 0;
    while (value[result]) result++;
    return result;
}
extern "C" int strcmp(const char *left, const char *right) {
    unsigned int index = 0;
    while (left[index] && left[index] == right[index]) index++;
    return (BYTE)left[index] - (BYTE)right[index];
}
// The only reachable upstream helper-process format is a literal plus one %s.
// Keep this support local to the fixture; helper processes are outside its test.
extern "C" int __cdecl __stdio_common_vsprintf(unsigned __int64 options, char *out,
    unsigned int capacity, const char *format, void *locale, va_list arguments) {
    unsigned int count = 0;
    (void)options;
    (void)locale;
    for (unsigned int index = 0; format[index]; index++) {
        if (format[index] == '%' && format[index + 1] == 's') {
            const char *value = va_arg(arguments, const char *);
            index++;
            for (unsigned int position = 0; value[position]; position++) {
                if (count + 1 >= capacity) return -1;
                out[count++] = value[position];
            }
        } else {
            if (count + 1 >= capacity) return -1;
            out[count++] = format[index];
        }
    }
    if (count >= capacity) return -1;
    out[count] = 0;
    return count;
}
extern "C" int __cdecl __stdio_common_vswprintf(unsigned __int64 options, WCHAR *out,
    unsigned int capacity, const WCHAR *format, void *locale, va_list arguments) {
    unsigned int count = 0;
    (void)options;
    (void)locale;
    for (unsigned int index = 0; format[index]; index++) {
        if (format[index] == '%' && format[index + 1] == 's') {
            const WCHAR *value = va_arg(arguments, const WCHAR *);
            index++;
            for (unsigned int position = 0; value[position]; position++) {
                if (count + 1 >= capacity) return -1;
                out[count++] = value[position];
            }
        } else {
            if (count + 1 >= capacity) return -1;
            out[count++] = format[index];
        }
    }
    if (count >= capacity) return -1;
    out[count] = 0;
    return count;
}
