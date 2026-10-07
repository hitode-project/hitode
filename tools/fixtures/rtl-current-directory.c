#include <windows.h>
#include <winternl.h>

typedef NTSTATUS (NTAPI *SET_DIRECTORY)(const UNICODE_STRING *);
typedef ULONG (NTAPI *GET_DIRECTORY)(ULONG, PWSTR);
static DWORD values[24];
static unsigned next;
static void record(DWORD value) { values[next++] = value; }
static void set(SET_DIRECTORY call, const WCHAR *path, USHORT bytes) {
    UNICODE_STRING name = { bytes, bytes + 2, (PWSTR)path };
    SetLastError(0x12345678);
    record((DWORD)call(&name));
    record(GetLastError());
}
void mainCRTStartup(void) {
    HMODULE nt = GetModuleHandleW(L"ntdll.dll");
    SET_DIRECTORY setdir = (SET_DIRECTORY)GetProcAddress(nt, "RtlSetCurrentDirectory_U");
    GET_DIRECTORY getdir = (GET_DIRECTORY)GetProcAddress(nt, "RtlGetCurrentDirectory_U");
    WCHAR old[1024], output[1024];
    DWORD base = GetCurrentDirectoryW(1024, old), count, written;
    HANDLE file;
    if (!setdir || !getdir || !base || base >= 1024) ExitProcess(1);
    CreateDirectoryW(L"cwd-\x5b57", NULL);
    file = CreateFileW(L"cwd-file", GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(2);
    CloseHandle(file);
    set(setdir, L"", 0);
    set(setdir, L"cwd-missing", 22);
    set(setdir, L"cwd-missing\\child", 34);
    set(setdir, L"cwd-file", 16);
    set(setdir, L"cwd-\x5b57", 10);
    count = GetCurrentDirectoryW(1024, output);
    record(count - base);
    record(output[count - 1] == 0x5b57);
    output[0] = 0x5555;
    SetLastError(0x12345678);
    record(getdir(0, NULL) - count * 2);
    record(getdir(count * 2, output) - count * 2);
    record(output[0]);
    record(getdir(count * 2 + 1, output) - count * 2);
    record(output[0]);
    record(getdir(count * 2 + 2, output) - count * 2);
    record(output[count] == 0);
    record(output[count - 1] == 0x5b57);
    record(GetLastError());
    set(setdir, L"..", 4);
    record(GetCurrentDirectoryW(1024, output) == base);
    if (next != 24) ExitProcess(3);
    file = CreateFileW(L"rtl-current-directory.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(4);
    if (!WriteFile(file, values, sizeof(values), &written, NULL) || written != sizeof(values)) ExitProcess(5);
    CloseHandle(file);
    ExitProcess(0);
}
