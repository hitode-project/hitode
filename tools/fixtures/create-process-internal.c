#include <windows.h>
#ifdef CHILD
void mainCRTStartup(void) { ExitProcess(37); }
#else
typedef BOOL (WINAPI *CREATE_INTERNAL)(HANDLE, LPCWSTR, LPWSTR, LPSECURITY_ATTRIBUTES, LPSECURITY_ATTRIBUTES, BOOL, DWORD, LPVOID, LPCWSTR, LPSTARTUPINFOW, LPPROCESS_INFORMATION, PHANDLE);
static DWORD values[16];
void mainCRTStartup(void) {
    HMODULE kernel = GetModuleHandleW(L"kernel32.dll");
    CREATE_INTERNAL create = (CREATE_INTERNAL)GetProcAddress(kernel, "CreateProcessInternalW");
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION info;
    HANDLE token = (HANDLE)0x12345678, file;
    DWORD exitcode = 0, written;
    if (!create) ExitProcess(1);
    info.hProcess = info.hThread = (HANDLE)0x55555555; info.dwProcessId = info.dwThreadId = 0x55555555;
    values[0] = create(NULL, L"fixture-missing.exe", NULL, NULL, NULL, FALSE, CREATE_SUSPENDED, NULL, NULL, &startup, &info, &token);
    values[1] = GetLastError();
    values[2] = !info.hProcess && !info.hThread && !info.dwProcessId && !info.dwThreadId;
    values[3] = token == (HANDLE)0x12345678;
    token = (HANDLE)0x12345678;
    values[4] = create(NULL, L"process-child.exe", NULL, NULL, NULL, FALSE, CREATE_SUSPENDED | CREATE_NO_WINDOW, NULL, NULL, &startup, &info, &token);
    if (!values[4]) ExitProcess(2);
    values[5] = info.hProcess != NULL; values[6] = info.hThread != NULL;
    values[7] = info.dwProcessId != 0; values[8] = info.dwThreadId != 0; values[9] = token == (HANDLE)0x12345678;
    values[10] = ResumeThread(info.hThread);
    values[11] = WaitForSingleObject(info.hProcess, 5000);
    values[12] = GetExitCodeProcess(info.hProcess, &exitcode); values[13] = exitcode;
    values[14] = CloseHandle(info.hThread); values[15] = CloseHandle(info.hProcess);
    file = CreateFileW(L"create-process-internal.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, sizeof(values), &written, NULL) || written != sizeof(values)) ExitProcess(3);
    CloseHandle(file); ExitProcess(0);
}
#endif
