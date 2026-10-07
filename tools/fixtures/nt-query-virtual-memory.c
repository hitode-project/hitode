/* Authored x86 basic VM query: status, LastError and output publication. */
#include <windows.h>
#ifndef CHILD
#define CHILD 0
#endif
typedef LONG (WINAPI *NT_QUERY)(HANDLE, const void *, LONG, void *, SIZE_T, SIZE_T *);

#if CHILD
void mainCRTStartup(void) { Sleep(INFINITE); ExitProcess(99); }
#else
static DWORD values[512], cursor;
static NT_QUERY query, query_zw;
static BYTE buffer[64];

static void fill(void *pointer, BYTE value, unsigned length) {
    BYTE *bytes = pointer;
    for (unsigned index = 0; index < length; index++) bytes[index] = value;
}
static int unchanged(const void *pointer, unsigned length) {
    const BYTE *bytes = pointer;
    for (unsigned index = 0; index < length; index++) if (bytes[index] != 0xa5) return 0;
    return 1;
}
static DWORD word(const void *pointer) {
    const BYTE *bytes = pointer;
    return bytes[0] | ((DWORD)bytes[1] << 8) | ((DWORD)bytes[2] << 16) | ((DWORD)bytes[3] << 24);
}
static int same(const BYTE *left, const BYTE *right, unsigned length) {
    for (unsigned index = 0; index < length; index++) if (left[index] != right[index]) return 0;
    return 1;
}
static void test(NT_QUERY function, HANDLE process, const void *address, LONG info_class,
        void *output, SIZE_T length, SIZE_T *returned, void *observed, unsigned observed_length) {
    MEMORY_BASIC_INFORMATION reference;
    SetLastError(0x13572468);
    LONG status = function(process, address, info_class, output, length, returned);
    values[cursor++] = (DWORD)status;
    values[cursor++] = GetLastError();
    values[cursor++] = returned ? word(returned) : 0;
    values[cursor++] = unchanged(observed, observed_length);
    values[cursor++] = status == 0 && output
        && VirtualQueryEx(process, address, &reference, sizeof(reference)) == sizeof(reference)
        && same(output, (BYTE *)&reference, sizeof(reference));
}
static void ordinary(NT_QUERY function, HANDLE process, const void *address, LONG info_class,
        SIZE_T length) {
    SIZE_T returned = 0x24681357;
    fill(buffer, 0xa5, sizeof(buffer));
    test(function, process, address, info_class, buffer, length, &returned, buffer, sizeof(buffer));
}
void mainCRTStartup(void) {
    /* Ensure the backend differential also exercises translated guest code. */
    volatile DWORD warm = 0;
    while (warm < 4096) warm++;
    HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
    query = (NT_QUERY)GetProcAddress(ntdll, "NtQueryVirtualMemory");
    query_zw = (NT_QUERY)GetProcAddress(ntdll, "ZwQueryVirtualMemory");
    BYTE *allocation = VirtualAlloc(NULL, 16384, MEM_RESERVE, PAGE_READWRITE);
    DWORD previous;
    if (!query || !query_zw || !allocation
        || !VirtualAlloc(allocation + 4096, 8192, MEM_COMMIT, PAGE_READWRITE)
        || !VirtualProtect(allocation + 8192, 4096, PAGE_EXECUTE_READ, &previous)) ExitProcess(101);
    ordinary(query, GetCurrentProcess(), allocation + 31, 0, 28);
    ordinary(query, GetCurrentProcess(), allocation + 4113, 0, 28);
    ordinary(query_zw, GetCurrentProcess(), allocation + 8291, 0, 28);
    ordinary(query, GetCurrentProcess(), allocation + 12289, 0, 28);
    ordinary(query, GetCurrentProcess(), allocation, 0, 32);
    ordinary(query, GetCurrentProcess(), allocation, 0, 27);
    ordinary(query, GetCurrentProcess(), allocation, 0, 0);
    ordinary(query, GetCurrentProcess(), allocation, -1, 28);
    ordinary(query, GetCurrentProcess(), (void *)~0u, 0, 28);
    ordinary(query, NULL, allocation, 0, 28);
    ordinary(query, (HANDLE)0x12345678, allocation, 0, 28);
    ordinary(query, GetCurrentThread(), allocation, 0, 28);
    SIZE_T returned = 0x24681357;
    fill(buffer, 0xa5, sizeof(buffer));
    test(query, GetCurrentProcess(), allocation, 0, NULL, 28, &returned, buffer, sizeof(buffer));
    fill(buffer, 0xa5, sizeof(buffer)); returned = 0x24681357;
    test(query, GetCurrentProcess(), allocation, 0, buffer + 1, 28, &returned, buffer, sizeof(buffer));
    fill(buffer, 0xa5, sizeof(buffer));
    test(query, GetCurrentProcess(), allocation, 0, buffer, 28, NULL, buffer, sizeof(buffer));
    BYTE length_buffer[8]; fill(length_buffer, 0xa5, sizeof(length_buffer));
    fill(buffer, 0xa5, sizeof(buffer));
    test(query, GetCurrentProcess(), allocation, 0, buffer, 28, (SIZE_T *)(length_buffer + 1), buffer, sizeof(buffer));
    fill(buffer, 0xa5, sizeof(buffer));
    SetLastError(0x13572468);
    values[cursor++] = (DWORD)query(GetCurrentProcess(), allocation, 0, buffer, 28, (SIZE_T *)1);
    values[cursor++] = GetLastError(); values[cursor++] = 0; values[cursor++] = unchanged(buffer, sizeof(buffer)); values[cursor++] = 0;
    fill(buffer, 0xa5, sizeof(buffer));
    test(query, GetCurrentProcess(), allocation, 0, buffer, 28, (SIZE_T *)(buffer + 12), buffer, sizeof(buffer));
    BYTE *read_only = VirtualAlloc(NULL, 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    if (!read_only) ExitProcess(102);
    fill(read_only, 0xa5, 64);
    if (!VirtualProtect(read_only, 4096, PAGE_READONLY, &previous)) ExitProcess(103);
    returned = 0x24681357;
    test(query, GetCurrentProcess(), allocation, 0, read_only, 28, &returned, read_only, 64);
    ordinary(query, (HANDLE)0x12345678, allocation, 0, 27);
    ordinary(query, (HANDLE)0x12345678, allocation, -1, 28);
    fill(buffer, 0xa5, sizeof(buffer)); returned = 0x24681357;
    SetLastError(0x13572468);
    values[cursor++] = (DWORD)query(GetCurrentProcess(), allocation, 0, (void *)1, 28, &returned);
    values[cursor++] = GetLastError(); values[cursor++] = returned; values[cursor++] = unchanged(buffer, sizeof(buffer)); values[cursor++] = 0;
    BYTE *edge = VirtualAlloc(NULL, 8192, MEM_RESERVE, PAGE_READWRITE);
    if (!edge || !VirtualAlloc(edge, 4096, MEM_COMMIT, PAGE_READWRITE)) ExitProcess(109);
    fill(edge + 4080, 0xa5, 16); returned = 0x24681357;
    test(query, GetCurrentProcess(), allocation, 0, edge + 4080, 28, &returned, edge + 4080, 16);
    fill(buffer, 0xa5, sizeof(buffer));
    test(query, GetCurrentProcess(), allocation, 0, buffer, 28, (SIZE_T *)read_only, buffer, sizeof(buffer));
    returned = 0x24681357;
    test(query, (HANDLE)0x12345678, allocation, 0, read_only, 28, &returned, read_only, 64);
    if (!VirtualFree(edge, 0, MEM_RELEASE)) ExitProcess(110);
    HANDLE own_query = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, GetCurrentProcessId());
    HANDLE own_limited = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, GetCurrentProcessId());
    HANDLE own_denied = OpenProcess(PROCESS_VM_OPERATION, FALSE, GetCurrentProcessId());
    if (!own_query || !own_limited || !own_denied) ExitProcess(104);
    ordinary(query, own_query, allocation + 4113, 0, 28);
    ordinary(query, own_limited, allocation + 4113, 0, 28);
    ordinary(query, own_denied, allocation + 4113, 0, 28);
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION child;
    if (!CreateProcessW(L"query-child.exe", NULL, NULL, NULL, FALSE, CREATE_SUSPENDED | CREATE_NO_WINDOW,
            NULL, NULL, &startup, &child)) ExitProcess(105);
    BYTE *remote = VirtualAllocEx(child.hProcess, NULL, 8192, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    HANDLE denied = NULL;
    if (!remote || !DuplicateHandle(GetCurrentProcess(), child.hProcess, GetCurrentProcess(),
            &denied, PROCESS_VM_OPERATION, FALSE, 0)) ExitProcess(106);
    ordinary(query, child.hProcess, remote + 37, 0, 28);
    ordinary(query, denied, remote, 0, 28);
    ordinary(query, child.hThread, remote, 0, 28);
    if (!TerminateProcess(child.hProcess, 37) || WaitForSingleObject(child.hProcess, 5000) != WAIT_OBJECT_0) ExitProcess(107);
    ordinary(query, child.hProcess, remote, 0, 28);
    values[cursor++] = CloseHandle(denied); values[cursor++] = CloseHandle(child.hThread);
    values[cursor++] = CloseHandle(child.hProcess);
    values[cursor++] = CloseHandle(own_query); values[cursor++] = CloseHandle(own_limited); values[cursor++] = CloseHandle(own_denied);
    values[cursor++] = VirtualFree(read_only, 0, MEM_RELEASE); values[cursor++] = VirtualFree(allocation, 0, MEM_RELEASE);
    DWORD written;
    HANDLE file = CreateFileW(L"nt-query-virtual-memory.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, cursor * 4, &written, NULL) || written != cursor * 4) ExitProcess(108);
    CloseHandle(file); ExitProcess(0);
}
#endif
