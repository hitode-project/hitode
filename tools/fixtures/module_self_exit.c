#include "win32_min.h"
API BOOL WINAPI GetExitCodeThread(HANDLE, DWORD *);
API HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
static DWORD events[128], count;
__declspec(dllexport) void __cdecl Record(DWORD value) {
    if (count < 128) events[count++] = value;
}
void mainCRTStartup(void) {
    HMODULE observer = LoadLibraryA("lifecycle_normal.dll");
    HMODULE self = LoadLibraryA("lifecycle_self.dll");
    THREADPROC entry;
    DWORD pass, code, written;
    HANDLE file, thread;
    if (!observer || !self || LoadLibraryA("lifecycle_self.dll") != self) ExitProcess(101);
    entry = (THREADPROC)GetProcAddress(self, "SelfExit");
    if (!entry) ExitProcess(102);
    for (pass = 0; pass < 2; pass++) {
        thread = CreateThread(NULL, 0, entry, (void *)self, 0, NULL);
        if (!thread || WaitForSingleObject(thread, 5000) != 0) ExitProcess(103);
        if (!GetExitCodeThread(thread, &code)) ExitProcess(104);
        CloseHandle(thread);
        Record(0xa0000000u | code);
        Record(0xb0000000u | (GetModuleHandleA("lifecycle_self.dll") != 0));
    }
    FreeLibrary(observer);
    file = CreateFileA("module-self-exit.bin", 0x40000000u, 0, NULL, 2, 0x80, 0);
    if (file == 0xffffffffu) ExitProcess(105);
    if (!WriteFile(file, events, count * 4, &written, NULL) || written != count * 4) ExitProcess(106);
    CloseHandle(file);
    ExitProcess(0);
}
