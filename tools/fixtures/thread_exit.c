#include "win32_min.h"
API HANDLE WINAPI GetCurrentProcess(void);
API HANDLE WINAPI GetCurrentThread(void);
API BOOL WINAPI DuplicateHandle(HANDLE, HANDLE, HANDLE, HANDLE *, DWORD, BOOL, DWORD);
API void WINAPI ExitThread(DWORD);
API BOOL WINAPI GetExitCodeThread(HANDLE, DWORD *);
API HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) DWORD __cdecl StartupStatus(void);
static HANDLE main_thread;
static DWORD startup;
static DWORD WINAPI worker(void *argument) {
    DWORD output[3], written;
    HANDLE file;
    (void)argument;
    Sleep(5);
    output[0] = startup;
    output[1] = WaitForSingleObject(main_thread, 0);
    output[2] = 0;
    GetExitCodeThread(main_thread, &output[2]);
    CloseHandle(main_thread);
    file = CreateFileA("thread-exit.bin", 0x40000000u, 0, NULL, 2, 0x80, 0);
    if (file == 0xffffffffu) return 101;
    if (!WriteFile(file, output, sizeof output, &written, NULL) || written != sizeof output) return 102;
    CloseHandle(file);
    return 19;
}
void mainCRTStartup(void) {
    HANDLE child;
    startup = StartupStatus();
    if (!DuplicateHandle(GetCurrentProcess(), GetCurrentThread(), GetCurrentProcess(), &main_thread, 0, 0, 2)) ExitProcess(103);
    child = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    if (!child) ExitProcess(104);
    CloseHandle(child);
    ExitThread(7);
}
