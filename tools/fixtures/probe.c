#include "win32_min.h"

static volatile DWORD initializers;
static int marker = 7;
static int *volatile marker_pointer = &marker;
static DWORD loaded_base;
#ifndef DYNAMIC_PROBE
__declspec(thread) static int thread_value = 37;
#endif

/* Declare TLS metadata normally supplied by the CRT. The callback and pointers
   also force real base relocations in each guest DLL. */
#pragma section(".tls", long, read, write)
#pragma section(".tls$ZZZ", long, read, write)
#pragma section(".CRT$XLA", long, read)
#pragma section(".CRT$XLB", long, read)
#pragma section(".CRT$XLZ", long, read)
#pragma section(".rdata$T", long, read)
__declspec(allocate(".tls")) char tls_start;
__declspec(allocate(".tls$ZZZ")) char tls_end;
DWORD _tls_index;
static void WINAPI tls_callback(void *module, DWORD reason, void *reserved) {
    (void)module; (void)reserved;
    if (reason == 1) initializers |= 1;
}
__declspec(allocate(".CRT$XLA")) TLSCALLBACK tls_first = NULL;
__declspec(allocate(".CRT$XLB")) TLSCALLBACK tls_function = tls_callback;
__declspec(allocate(".CRT$XLZ")) TLSCALLBACK tls_last = NULL;
__declspec(allocate(".rdata$T")) const TLS_DIRECTORY _tls_used = {
    &tls_start, &tls_end, &_tls_index, &tls_first + 1, 0, 0
};
#pragma comment(linker, "/include:__tls_used")
#pragma comment(linker, "/include:_tls_function")

BOOL WINAPI DllMain(HANDLE module, DWORD reason, void *reserved) {
    (void)reserved;
    if (reason == 1) { initializers |= 2; loaded_base = module; }
    return 1;
}

int __cdecl ProbeAdd(int value) {
    if (marker_pointer != &marker || initializers != 3) return -1;
    return value + *marker_pointer + (int)initializers;
}
int __cdecl ProbeReadTls(void) {
#ifdef DYNAMIC_PROBE
    return 37;
#else
    return thread_value;
#endif
}
void __cdecl ProbeSetTls(int value) {
#ifndef DYNAMIC_PROBE
    thread_value = value;
#else
    (void)value;
#endif
}
DWORD __cdecl ProbeBase(void) { return loaded_base; }
