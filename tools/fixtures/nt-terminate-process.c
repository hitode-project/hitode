/* Authored NT termination differential: 44 parent and eleven NULL-call statistics. */
#include <windows.h>
#ifndef MODE
#define MODE 0
#endif
typedef LONG (WINAPI *NT_TERMINATE)(HANDLE, LONG);

static void publish(const WCHAR *name, const DWORD *values, DWORD count) {
    DWORD written;
    HANDLE file = CreateFileW(name, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, count * 4, &written, NULL)
        || written != count * 4) ExitProcess(101);
    CloseHandle(file);
}

#if MODE == 4
static DWORD values[11];
static DWORD WINAPI worker(void *argument) { return 99; }
void mainCRTStartup(void) {
    NT_TERMINATE terminate = (NT_TERMINATE)GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtTerminateProcess");
    DWORD written, code = 0;
    HANDLE first = CreateThread(NULL, 0, worker, NULL, CREATE_SUSPENDED, NULL), file;
    values[0] = first != NULL;
    if (!terminate || !first) ExitProcess(102);
    file = CreateFileW(L"nt-terminate-null.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, 4, &written, NULL) || written != 4)
        ExitProcess(110);
    SetLastError(0x13572468);
    values[1] = (DWORD)terminate(NULL, (LONG)0x81234567);
    values[2] = GetLastError();
    values[3] = WaitForSingleObject(first, 5000);
    values[4] = GetExitCodeThread(first, &code);
    values[5] = code;
    values[6] = GetExitCodeProcess(GetCurrentProcess(), &code);
    values[7] = code;
    values[8] = WaitForSingleObject(GetCurrentProcess(), 0);
    if (!WriteFile(file, &values[1], 32, &written, NULL) || written != 32) ExitProcess(112);
    SetLastError(0x13572468);
    values[9] = (DWORD)terminate(NULL, 73);
    values[10] = GetLastError();
    WriteFile(file, &values[9], 8, &written, NULL);
    CloseHandle(file);
    ExitProcess(111);
}
#elif MODE == 2 || MODE == 3 || MODE == 5
void mainCRTStartup(void) {
    NT_TERMINATE terminate = (NT_TERMINATE)GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtTerminateProcess");
    HANDLE process = GetCurrentProcess();
#if MODE == 3
    process = OpenProcess(PROCESS_TERMINATE, FALSE, GetCurrentProcessId());
#elif MODE == 5
    DWORD marker = 1, written;
    HANDLE file = CreateFileW(L"nt-terminate-single-null.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, &marker, 4, &written, NULL) || written != 4)
        ExitProcess(113);
    process = NULL;
#endif
    if (!terminate) ExitProcess(103);
    LONG returned = terminate(process, MODE == 2 ? 41 : MODE == 3 ? 42 : 43);
#if MODE == 5
    WriteFile(file, &returned, 4, &written, NULL);
#endif
    ExitProcess(104);
}
#elif MODE == 1
void mainCRTStartup(void) { ExitProcess(105); }
#else
static DWORD values[44];
static unsigned cursor;
static void child(const WCHAR *filename, DWORD expected, DWORD thread_expected) {
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION info;
    DWORD code = 0;
    values[cursor++] = CreateProcessW(filename, NULL, NULL, NULL, FALSE, CREATE_NO_WINDOW,
        NULL, NULL, &startup, &info);
    if (!values[cursor - 1]) ExitProcess(106);
    values[cursor++] = WaitForSingleObject(info.hProcess, 5000) == WAIT_OBJECT_0;
    values[cursor++] = GetExitCodeProcess(info.hProcess, &code) && code == expected;
    values[cursor++] = GetExitCodeThread(info.hThread, &code) && code == thread_expected;
    CloseHandle(info.hThread);
    CloseHandle(info.hProcess);
}
static void result(NT_TERMINATE terminate, HANDLE process) {
    SetLastError(0x13572468);
    values[cursor++] = (DWORD)terminate(process, (LONG)0x81234567);
    values[cursor++] = GetLastError();
}
void mainCRTStartup(void) {
    NT_TERMINATE terminate = (NT_TERMINATE)GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtTerminateProcess");
    NT_TERMINATE terminate_zw = (NT_TERMINATE)GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "ZwTerminateProcess");
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION info;
    HANDLE restricted = NULL, self_denied;
    DWORD code = 0;
    values[cursor++] = CreateProcessW(L"process-child.exe", NULL, NULL, NULL, FALSE,
        CREATE_SUSPENDED | CREATE_NO_WINDOW, NULL, NULL, &startup, &info);
    if (!terminate || !terminate_zw || !values[0]) ExitProcess(107);
    values[cursor++] = DuplicateHandle(GetCurrentProcess(), info.hProcess, GetCurrentProcess(),
        &restricted, PROCESS_QUERY_INFORMATION | SYNCHRONIZE, FALSE, 0);
    if (!restricted) { TerminateProcess(info.hProcess, 108); ExitProcess(108); }
    result(terminate, (HANDLE)0x12345678);
    result(terminate, info.hThread);
    result(terminate, GetCurrentThread());
    result(terminate, restricted);
    values[cursor++] = GetExitCodeProcess(info.hProcess, &code);
    values[cursor++] = code == STILL_ACTIVE;
    result(terminate_zw, info.hProcess);
    values[cursor++] = WaitForSingleObject(info.hProcess, 5000) == WAIT_OBJECT_0;
    values[cursor++] = GetExitCodeProcess(info.hProcess, &code);
    values[cursor++] = code == 0x81234567;
    result(terminate, info.hProcess);
    values[cursor++] = GetExitCodeProcess(info.hProcess, &code);
    values[cursor++] = code == 0x81234567;
    values[cursor++] = CloseHandle(restricted);
    values[cursor++] = CloseHandle(info.hThread);
    values[cursor++] = CloseHandle(info.hProcess);
    self_denied = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, GetCurrentProcessId());
    values[cursor++] = self_denied != NULL;
    result(terminate, self_denied);
    values[cursor++] = CloseHandle(self_denied);
    child(L"self-pseudo.exe", 41, 41);
    child(L"self-owned.exe", 42, 42);
    child(L"null-process.exe", 0x81234567, 73);
    child(L"single-null.exe", 43, 104);
    if (cursor != 44) ExitProcess(109);
    publish(L"nt-terminate-process.bin", values, 44);
    ExitProcess(0);
}
#endif
