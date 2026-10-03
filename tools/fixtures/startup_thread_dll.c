#include "win32_min.h"
static HANDLE child;
static volatile DWORD phase, early;
static DWORD WINAPI worker(void *argument) {
    (void)argument;
    if (phase != 2) early = 1;
    phase = 3;
    return 0;
}
BOOL WINAPI DllMain(HMODULE module, DWORD reason, void *reserved) {
    (void)module; (void)reserved;
    if (reason == 1) {
        phase = 1;
        child = CreateThread(NULL, 0, worker, NULL, 0, NULL);
        if (!child) return 0;
        Sleep(20);
        if (phase != 1) early = 1;
        phase = 2;
    }
    return 1;
}
__declspec(dllexport) DWORD __cdecl StartupStatus(void) {
    WaitForSingleObject(child, INFINITE);
    CloseHandle(child);
    return early || phase != 3;
}
