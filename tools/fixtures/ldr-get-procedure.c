#include <windows.h>
#include <winternl.h>
typedef NTSTATUS (NTAPI *LOOKUP)(PVOID, const ANSI_STRING *, ULONG, PVOID *);
static DWORD values[18];
void mainCRTStartup(void) {
    HMODULE nt = GetModuleHandleW(L"ntdll.dll"), kernel = GetModuleHandleW(L"kernel32.dll");
    LOOKUP lookup = (LOOKUP)GetProcAddress(nt, "LdrGetProcedureAddress");
    ANSI_STRING name = {6, 7, "GetACP"}, prefix = {6, 14, "GetACP-unused"};
    ANSI_STRING missing = {18, 19, "NoSuchFixtureEntry"};
    ANSI_STRING rtl = {24, 25, "RtlSetCurrentDirectory_U"};
    PVOID result = NULL, expected = (PVOID)GetProcAddress(kernel, "GetACP");
    HANDLE file; DWORD written;
    if (!lookup || !expected) ExitProcess(1);
    SetLastError(0x12345678);
    values[0] = (DWORD)lookup(kernel, &name, 0, &result);
    values[1] = result == expected;
    values[2] = result && ((UINT (WINAPI *)(void))result)() == GetACP();
    values[3] = GetLastError();
    values[4] = (DWORD)lookup(kernel, &prefix, 0, &result); values[5] = result == expected;
    values[6] = (DWORD)lookup(kernel, &name, 65535, &result); values[7] = result == expected;
    result = (PVOID)0x12345678;
    values[8] = (DWORD)lookup(kernel, &missing, 0, &result); values[9] = result == (PVOID)0x12345678;
    values[10] = GetLastError();
    result = (PVOID)0x12345678;
    values[11] = (DWORD)lookup(kernel, NULL, 0, &result);
    values[16] = result == NULL;
    values[17] = (DWORD)lookup(kernel, NULL, 65535, &result);
    values[12] = (DWORD)lookup(nt, &rtl, 0, &result);
    values[13] = result == (PVOID)GetProcAddress(nt, "RtlSetCurrentDirectory_U");
    values[14] = GetLastError(); values[15] = name.Length == 6 && name.MaximumLength == 7;
    file = CreateFileW(L"ldr-get-procedure.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, sizeof(values), &written, NULL) || written != sizeof(values)) ExitProcess(2);
    CloseHandle(file); ExitProcess(0);
}
