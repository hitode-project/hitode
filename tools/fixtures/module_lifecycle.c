#include "win32_min.h"

API BOOL WINAPI GetModuleHandleExA(DWORD, const char *, HMODULE *);
API BOOL WINAPI DisableThreadLibraryCalls(HMODULE);
API DWORD WINAPI GetLastError(void);
API void WINAPI SetLastError(DWORD);
API HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);

static DWORD observations[512], count;
static HANDLE tls_ready, tls_go, tls_done, tls_exit;
static HANDLE load_ready[2], load_done[2], load_go, load_exit;
static HMODULE load_modules[2];
__declspec(dllexport) void __cdecl Record(DWORD value) {
    if (count < 512) observations[count++] = value;
}
static void mark(DWORD step) { Record(0xf0000000u | step); }
static void loaded(const char *name) { Record(GetModuleHandleA(name) != 0); }
static DWORD WINAPI tls_worker(void *argument) {
    HMODULE module;
    int (__cdecl *value)(void);
    (void)argument;
    SetEvent(tls_ready); WaitForSingleObject(tls_go, INFINITE);
    module = GetModuleHandleA("lifecycle_tls.dll");
    value = (int (__cdecl *)(void))GetProcAddress(module, "TlsValue");
    Record(value ? value() : 0);
    SetEvent(tls_done); WaitForSingleObject(tls_exit, INFINITE);
    return 0;
}
static DWORD WINAPI load_worker(void *argument) {
    DWORD index = (DWORD)argument;
    SetEvent(load_ready[index]); WaitForSingleObject(load_go, INFINITE);
    load_modules[index] = LoadLibraryA("lifecycle_slow.dll");
    SetEvent(load_done[index]); WaitForSingleObject(load_exit, INFINITE);
    return 0;
}
void mainCRTStartup(void) {
    HMODULE first, second, found, dep, parent;
    HANDLE file, worker, loaders[2];
    DWORD written;
    mark(1);
    first = LoadLibraryA("lifecycle_normal.dll"); Record(first != 0);
    if (first) {
        int (__cdecl *value)(void) = (int (__cdecl *)(void))GetProcAddress(first, "Dependency");
        DWORD i, sum = 0;
        for (i = 0; i < 96; ++i) sum += value ? value() : 0;
        Record(sum);
    }
    second = LoadLibraryA("lifecycle_normal.dll"); Record(first == second);
    found = 0;
    Record(GetModuleHandleExA(2, "lifecycle_normal.dll", &found)); Record(found == first);
    Record(FreeLibrary(first)); loaded("lifecycle_normal.dll");
    Record(FreeLibrary(second)); loaded("lifecycle_normal.dll");
    mark(2);
    first = LoadLibraryA("lifecycle_normal.dll");
    Record(GetModuleHandleExA(0, "lifecycle_normal.dll", &found));
    Record(found == first);
    Record(FreeLibrary(first)); loaded("lifecycle_normal.dll");
    Record(FreeLibrary(found)); loaded("lifecycle_normal.dll");
    mark(3);
    dep = LoadLibraryA("lifecycle_dep.dll"); Record(dep != 0);
    parent = LoadLibraryA("lifecycle_parent.dll"); Record(parent != 0);
    Record(FreeLibrary(parent)); loaded("lifecycle_parent.dll"); loaded("lifecycle_dep.dll");
    Record(FreeLibrary(dep)); loaded("lifecycle_dep.dll");
    mark(4);
    SetLastError(0);
    first = LoadLibraryA("lifecycle_fail.dll");
    Record(first != 0); Record(GetLastError()); loaded("lifecycle_fail.dll");
    mark(5);
    tls_ready = CreateEventA(NULL, 0, 0, NULL); tls_go = CreateEventA(NULL, 0, 0, NULL);
    tls_done = CreateEventA(NULL, 0, 0, NULL); tls_exit = CreateEventA(NULL, 0, 0, NULL);
    worker = CreateThread(NULL, 0, tls_worker, NULL, 0, NULL);
    WaitForSingleObject(tls_ready, INFINITE);
    first = LoadLibraryA("lifecycle_tls.dll"); Record(first != 0);
    if (first) {
        int (__cdecl *value)(void) = (int (__cdecl *)(void))GetProcAddress(first, "TlsValue");
        void (__cdecl *set)(int) = (void (__cdecl *)(int))GetProcAddress(first, "TlsSet");
        Record(value ? value() : 0); if (set) set(73);
        SetEvent(tls_go); WaitForSingleObject(tls_done, INFINITE);
        Record(value ? value() : 0);
    }
    Record(DisableThreadLibraryCalls(first));
    Record(FreeLibrary(first)); loaded("lifecycle_tls.dll");
    SetEvent(tls_exit); WaitForSingleObject(worker, INFINITE); CloseHandle(worker);
    CloseHandle(tls_ready); CloseHandle(tls_go); CloseHandle(tls_done); CloseHandle(tls_exit);
    mark(6);
    first = LoadLibraryA("lifecycle_normal.dll"); Record(first != 0);
    found = 123;
    Record(GetModuleHandleExA(3, "lifecycle_normal.dll", &found)); Record(found);
    Record(GetLastError());
    found = 0;
    Record(GetModuleHandleExA(4 | 2, (const char *)first, &found)); Record(found == first);
    Record(DisableThreadLibraryCalls(first));
    Record(FreeLibrary(first)); loaded("lifecycle_normal.dll");
    mark(9);
    load_go = CreateEventA(NULL, 1, 0, NULL); load_exit = CreateEventA(NULL, 1, 0, NULL);
    for (written = 0; written < 2; ++written) {
        load_ready[written] = CreateEventA(NULL, 0, 0, NULL);
        load_done[written] = CreateEventA(NULL, 0, 0, NULL);
        loaders[written] = CreateThread(NULL, 0, load_worker, (void *)written, 0, NULL);
        WaitForSingleObject(load_ready[written], INFINITE);
    }
    SetEvent(load_go);
    for (written = 0; written < 2; ++written) WaitForSingleObject(load_done[written], INFINITE);
    /* Keep the record buffer single-writer; concurrent worker Record calls can
       lose an entry on native Windows even when both loads succeed. */
    Record(load_modules[0] != 0); Record(load_modules[1] != 0);
    Record(load_modules[0] == load_modules[1]);
    Record(FreeLibrary(load_modules[0])); Record(FreeLibrary(load_modules[1]));
    loaded("lifecycle_slow.dll");
    SetEvent(load_exit);
    for (written = 0; written < 2; ++written) {
        WaitForSingleObject(loaders[written], INFINITE); CloseHandle(loaders[written]);
        CloseHandle(load_ready[written]); CloseHandle(load_done[written]);
    }
    CloseHandle(load_go); CloseHandle(load_exit);
    mark(7);
    first = LoadLibraryA("lifecycle_normal.dll");
    Record(GetModuleHandleExA(1, "lifecycle_normal.dll", &found)); Record(found == first);
    Record(FreeLibrary(first)); Record(FreeLibrary(first)); loaded("lifecycle_normal.dll");
    mark(8);
    SetLastError(0); Record(FreeLibrary(0)); Record(GetLastError());
    file = CreateFileA("module-lifecycle.bin", 0x40000000u, 0, NULL, 2, 0x80, 0);
    if (file == 0xffffffffu) ExitProcess(101);
    if (!WriteFile(file, observations, count * 4, &written, NULL) || written != count * 4) ExitProcess(102);
    CloseHandle(file);
    ExitProcess(0);
}
