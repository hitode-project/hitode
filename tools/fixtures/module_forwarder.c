#include "win32_min.h"
API DWORD WINAPI GetLastError(void);
API void WINAPI SetLastError(DWORD);
API HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
static DWORD events[128], count;
__declspec(dllexport) void __cdecl Record(DWORD value) {
    if (count < 128) events[count++] = value;
}
static void failed(HMODULE module, const char *name) {
    void *address;
    SetLastError(0);
    address = GetProcAddress(module, name);
    Record(address != NULL);
    Record(GetLastError());
    Record(GetModuleHandleA("lifecycle_fail.dll") != 0);
}
void mainCRTStartup(void) {
    typedef int (__cdecl *Dependency)(void);
    HMODULE root = LoadLibraryA("lifecycle_forward");
    Dependency dep;
    HANDLE file;
    DWORD written;
    if (!root) ExitProcess(101);
    Record(GetModuleHandleA("lifecycle_dep.dll") != 0);
    dep = (Dependency)GetProcAddress(root, "Later");
    if (!dep) ExitProcess(102);
    Record(dep());
    Record(GetModuleHandleA("lifecycle_dep.dll") != 0);
    Record(GetProcAddress(root, "Later") == (void *)dep);
    failed(root, "Failed");
    failed(root, "FailedTree");
    Record(GetModuleHandleA("lifecycle_failtree.dll") != 0);
    Record(GetModuleHandleA("lifecycle_tls.dll") != 0);
    failed(root, "Absent");
    Record(GetModuleHandleA("lifecycle_forward.dll") == root);
    FreeLibrary(root);
    Record(GetModuleHandleA("lifecycle_forward.dll") != 0);
    Record(GetModuleHandleA("lifecycle_dep.dll") != 0);
    file = CreateFileA("module-forwarder.bin", 0x40000000u, 0, NULL, 2, 0x80, 0);
    if (file == 0xffffffffu) ExitProcess(103);
    if (!WriteFile(file, events, count * 4, &written, NULL) || written != count * 4) ExitProcess(104);
    CloseHandle(file);
    ExitProcess(0);
}
