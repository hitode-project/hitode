#include "win32_min.h"
#ifndef MODULE_ID
#define MODULE_ID 1
#endif

#ifdef WITH_FORWARD_IMPORT
__declspec(dllimport) int __cdecl AbsentGuest(void);
__declspec(dllexport) int __cdecl ForwardImport(void) { return AbsentGuest(); }
#endif

#ifdef WITH_TLS_DEPENDENCY
__declspec(dllimport) int __cdecl TlsValue(void);
__declspec(dllexport) int __cdecl LinkedValue(void) { return TlsValue(); }
#endif

#ifdef SELF_EXIT
API __declspec(noreturn) void WINAPI FreeLibraryAndExitThread(HMODULE, DWORD);
__declspec(dllexport) DWORD WINAPI SelfExit(void *argument) {
    volatile DWORD count = 0;
    while (count < 512) count++;
    FreeLibraryAndExitThread((HMODULE)argument, 23);
}
#endif
typedef void (__cdecl *Recorder)(DWORD);
static void record(DWORD kind, DWORD reason, void *reserved) {
    Recorder output = (Recorder)GetProcAddress(GetModuleHandleA(NULL), "Record");
    if (output) output((MODULE_ID << 16) | (kind << 8) | reason | (reserved ? 0x80 : 0));
}
#ifdef WITH_DEPENDENCY
__declspec(dllimport) int __cdecl Dependency(void);
#else
__declspec(dllexport) int __cdecl Dependency(void) { return MODULE_ID; }
#endif
#ifdef WITH_TLS
__declspec(thread) static volatile int thread_value = 37;
__declspec(dllexport) int __cdecl TlsValue(void) { return thread_value; }
__declspec(dllexport) void __cdecl TlsSet(int value) { thread_value = value; }
#pragma section(".tls", long, read, write)
#pragma section(".tls$ZZZ", long, read, write)
#pragma section(".CRT$XLA", long, read)
#pragma section(".CRT$XLB", long, read)
#pragma section(".CRT$XLZ", long, read)
#pragma section(".rdata$T", long, read)
__declspec(allocate(".tls")) DWORD tls_start = 37;
__declspec(allocate(".tls$ZZZ")) char tls_end;
DWORD _tls_index;
static void WINAPI callback(void *module, DWORD reason, void *reserved) {
    (void)module; record(1, reason, reserved);
    if (reason == 1 && thread_value != 37) record(0xfe, reason, reserved);
}
__declspec(allocate(".CRT$XLA")) TLSCALLBACK tls_first = NULL;
__declspec(allocate(".CRT$XLB")) TLSCALLBACK tls_function = callback;
__declspec(allocate(".CRT$XLZ")) TLSCALLBACK tls_last = NULL;
__declspec(allocate(".rdata$T")) const TLS_DIRECTORY _tls_used = {
    &tls_start, &tls_end, &_tls_index, &tls_first + 1, 0, 0
};
#pragma comment(linker, "/include:__tls_used")
#pragma comment(linker, "/include:_tls_function")
#endif
BOOL WINAPI DllMain(HMODULE module, DWORD reason, void *reserved) {
    (void)module; record(2, reason, reserved);
#ifdef SELF_EXIT
    /* Detach is resumable; the worker must never return into this image. */
    if (reason == 0) Sleep(1);
#endif
#ifdef SLOW_ATTACH
    if (reason == 1) { Sleep(20); record(3, reason, reserved); }
#endif
#ifdef WITH_DEPENDENCY
    if (reason == 1 && Dependency() != 2) return 0;
#endif
#ifdef FAIL_ATTACH
    if (reason == 1) return 0;
#endif
    return 1;
}
