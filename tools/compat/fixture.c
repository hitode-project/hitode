/* Owned x86 oracle: static/dynamic imports, strings, LastError, threads and child. */
#include <windows.h>
static DWORD WINAPI worker(LPVOID unused) {
    (void)unused; SetLastError(123); GetCurrentProcessId();
    HANDLE event = CreateEventW(NULL, TRUE, FALSE, L"api_diff_fixture_wide");
    if (event) CloseHandle(event);
    return 0;
}
void mainCRTStartup(void) {
    WCHAR child_flag[2];
    if (GetEnvironmentVariableW(L"ADV_API_DIFF_CHILD", child_flag, 2)) ExitProcess(0);
    SetLastError(731);
    DWORD tick = GetTickCount();
    if (GetLastError() != 731 || !tick) ExitProcess(9);
    HANDLE event = CreateEventA(NULL, TRUE, FALSE, "api_diff_fixture_ascii");
    if (!event) ExitProcess(8);
    SetEvent(event); WaitForSingleObject(event, 0); CloseHandle(event);
    HMODULE module = LoadLibraryW(L"winmm.dll");
    DWORD (WINAPI *clock)(void) = (DWORD (WINAPI *)(void))GetProcAddress(module, "timeGetTime");
    if (!clock || !clock()) ExitProcess(7);
    BYTE (WINAPI *lock_heap)(HANDLE) = (BYTE (WINAPI *)(HANDLE))GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "RtlLockHeap");
    BYTE (WINAPI *unlock_heap)(HANDLE) = (BYTE (WINAPI *)(HANDLE))GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "RtlUnlockHeap");
    HANDLE heap = GetProcessHeap();
    if (!lock_heap || !unlock_heap || !lock_heap(heap) || !unlock_heap(heap)) ExitProcess(5);
    HANDLE thread = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    WaitForSingleObject(thread, 2000); CloseHandle(thread);
    WCHAR path[1024]; GetModuleFileNameW(NULL, path, 1024);
    STARTUPINFOW startup = {0}; startup.cb = sizeof(startup);
    PROCESS_INFORMATION child = {0};
    SetEnvironmentVariableW(L"ADV_API_DIFF_CHILD", L"1");
    if (!CreateProcessW(path, NULL, NULL, NULL, FALSE, CREATE_SUSPENDED, NULL, NULL, &startup, &child)) ExitProcess(6);
    SetEnvironmentVariableW(L"ADV_API_DIFF_CHILD", NULL);
    ResumeThread(child.hThread); WaitForSingleObject(child.hProcess, 2000);
    CloseHandle(child.hThread); CloseHandle(child.hProcess);
    Sleep(350); ExitProcess(0);
}
