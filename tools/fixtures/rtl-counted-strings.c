#include <windows.h>
#include <winternl.h>
typedef BOOLEAN (NTAPI *EQUAL)(const UNICODE_STRING *, const UNICODE_STRING *, BOOLEAN);
typedef VOID (NTAPI *INIT)(PANSI_STRING, PCSTR);
static DWORD values[24];
static unsigned next;
static void record(DWORD value) { values[next++] = value; }
void mainCRTStartup(void) {
    HMODULE nt = GetModuleHandleW(L"ntdll.dll");
    EQUAL equal = (EQUAL)GetProcAddress(nt, "RtlEqualUnicodeString");
    INIT init = (INIT)GetProcAddress(nt, "RtlInitAnsiString");
    UNICODE_STRING a = {4, 6, L"Ab"}, b = {4, 6, L"AB"}, c = {2, 4, L"\x00c4"}, d = {2, 4, L"\x00e4"};
    WCHAR nul_a[] = {'A', 0, 'B'}, nul_b[] = {'A', 0, 'C'}, surrogate[] = {0xd800};
    UNICODE_STRING e = {6, 6, nul_a}, f = {6, 6, nul_b}, empty = {0, 0, NULL};
    UNICODE_STRING raw = {2, 2, surrogate}, sharp = {2, 4, L"\x00df"}, ss = {4, 6, L"SS"};
    ANSI_STRING ansi;
    const char *name = "api", *zero = "";
    HANDLE file; DWORD written;
    if (!equal || !init) ExitProcess(1);
    SetLastError(0x12345678);
    record(equal(&a, &b, FALSE)); record(equal(&a, &b, TRUE)); record(equal(&a, &a, FALSE));
    record(equal(&c, &d, TRUE)); record(equal(&c, &d, FALSE)); record(equal(&e, &f, TRUE));
    record(equal(&a, &c, TRUE)); record(equal(&empty, &empty, FALSE)); record(equal(&raw, &raw, TRUE));
    record(equal(&sharp, &ss, TRUE)); a.Length = 2; b.Length = 2; record(equal(&a, &b, FALSE));
    a.Length = 4; b.Length = 4; record(equal(&a, &b, (BOOLEAN)0x100)); record(GetLastError());
    init(&ansi, name); record(ansi.Length); record(ansi.MaximumLength); record(ansi.Buffer == name);
    init(&ansi, NULL); record(ansi.Length); record(ansi.MaximumLength); record(ansi.Buffer == NULL);
    init(&ansi, zero); record(ansi.Length); record(ansi.MaximumLength); record(ansi.Buffer == zero);
    record(GetLastError()); record(a.Length == 4 && b.Length == 4);
    if (next != 24) ExitProcess(2);
    file = CreateFileW(L"rtl-counted-strings.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, sizeof(values), &written, NULL) || written != sizeof(values)) ExitProcess(3);
    CloseHandle(file); ExitProcess(0);
}
