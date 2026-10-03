#include "win32_min.h"
API BOOL WINAPI DisableThreadLibraryCalls(HMODULE);
API void WINAPI ExitThread(DWORD);
API BOOL WINAPI GetExitCodeThread(HANDLE, DWORD *);
API HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);

static DWORD observations[512], count, flush_on_detach;
static void flush(void) {
    DWORD written;
    HANDLE file = CreateFileA("module-notifications.bin", 0x40000000u, 0, NULL, 2, 0x80, 0);
    if (file != 0xffffffffu) {
        WriteFile(file, observations, count * 4, &written, NULL);
        CloseHandle(file);
    }
}
__declspec(dllexport) void __cdecl Record(DWORD value) {
    if (count < 512) observations[count++] = value;
    // The first loaded DLL detaches last. Flush from the executable's callback
    // so the file contains process notifications as well as thread notifications.
    if (flush_on_detach && (value & ~0x80u) == 0x10200u) flush();
}
static DWORD WINAPI worker(void *argument) {
    Record(0xee000000u | (DWORD)argument);
    if ((DWORD)argument == 3) ExitThread(77);
    return 23;
}
static void thread(DWORD argument) {
    DWORD result = 0;
    HANDLE handle = CreateThread(NULL, 0, worker, (void *)argument, 0, NULL);
    WaitForSingleObject(handle, INFINITE);
    Record(GetExitCodeThread(handle, &result)); Record(result); CloseHandle(handle);
}
void mainCRTStartup(void) {
    HMODULE normal = LoadLibraryA("lifecycle_normal.dll");
    HMODULE tls = LoadLibraryA("lifecycle_tls.dll");
    Record(0xf0000001u); thread(1);
    Record(0xf0000002u);
    Record(DisableThreadLibraryCalls(normal)); Record(DisableThreadLibraryCalls(tls)); thread(2);
    Record(0xf0000003u); thread(3);
    Record(FreeLibrary(tls)); Record(FreeLibrary(normal));
    Record(0xf0000004u);
    normal = LoadLibraryA("lifecycle_normal.dll"); tls = LoadLibraryA("lifecycle_tls.dll");
    Record(normal != 0); Record(tls != 0);
    flush_on_detach = 1;
    ExitProcess(0);
}
