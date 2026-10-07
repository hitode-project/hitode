/* Authored suspended-child NT free differential; emits 75 DWORD statistics. */
#include <windows.h>

#ifdef CHILD
void mainCRTStartup(void) { ExitProcess(37); }
#else
typedef LONG (WINAPI *NT_FREE)(HANDLE, PVOID *, PSIZE_T, ULONG);
static DWORD values[75];
static unsigned cursor;

static void check_free(NT_FREE release, HANDLE process, PVOID base, SIZE_T size,
                       ULONG flags, PVOID expected_base, SIZE_T expected_size) {
    SetLastError(0x13572468);
    values[cursor++] = (DWORD)release(process, &base, &size, flags);
    values[cursor++] = base == expected_base;
    values[cursor++] = size == expected_size;
    values[cursor++] = GetLastError();
}

void mainCRTStartup(void) {
    NT_FREE release = (NT_FREE)GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtFreeVirtualMemory");
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION info;
    MEMORY_BASIC_INFORMATION memory;
    HANDLE restricted = NULL, file;
    PVOID owner, base;
    SIZE_T size, transferred = 0;
    DWORD marker = 0x24681357, checked = 0, exitcode = 0, written;

    values[cursor++] = CreateProcessW(L"process-child.exe", NULL, NULL, NULL, FALSE,
        CREATE_SUSPENDED | CREATE_NO_WINDOW, NULL, NULL, &startup, &info);
    if (!release || !values[0]) ExitProcess(1);
    values[cursor++] = DuplicateHandle(GetCurrentProcess(), info.hProcess,
        GetCurrentProcess(), &restricted, PROCESS_VM_READ, FALSE, 0);
    owner = VirtualAllocEx(info.hProcess, NULL, 3 * 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    values[cursor++] = owner != NULL;
    if (!values[1] || !owner) { TerminateProcess(info.hProcess, 1); ExitProcess(2); }
    values[cursor++] = WriteProcessMemory(info.hProcess, owner, &marker, 4, &transferred);
    values[cursor++] = transferred == 4;

    base = (BYTE *)owner + 3;
    check_free(release, (HANDLE)0x12345678, base, 7, MEM_DECOMMIT, base, 7);
    check_free(release, restricted, base, 7, MEM_DECOMMIT, base, 7);
    check_free(release, info.hProcess, base, 7, 0, base, 7);
    base = (BYTE *)owner + 4097;
    check_free(release, info.hProcess, base, 0, MEM_DECOMMIT, base, 0);
    base = (BYTE *)owner + 8192;
    check_free(release, info.hProcess, base, 8192, MEM_DECOMMIT, base, 8192);

    size = 1;
    SetLastError(0x13572468);
    values[cursor++] = (DWORD)release(info.hProcess, NULL, &size, MEM_DECOMMIT);
    values[cursor++] = size == 1;
    values[cursor++] = GetLastError();
    base = owner;
    SetLastError(0x13572468);
    values[cursor++] = (DWORD)release(info.hProcess, &base, NULL, MEM_DECOMMIT);
    values[cursor++] = base == owner;
    values[cursor++] = GetLastError();
    values[cursor++] = ReadProcessMemory(info.hProcess, owner, &checked, 4, &transferred);
    values[cursor++] = transferred == 4;
    values[cursor++] = checked == marker;

    base = (BYTE *)owner + 4095;
    check_free(release, info.hProcess, base, 2, MEM_DECOMMIT, owner, 8192);
    values[cursor++] = VirtualQueryEx(info.hProcess, owner, &memory, sizeof(memory)) == sizeof(memory);
    values[cursor++] = memory.State == MEM_RESERVE;
    values[cursor++] = memory.Protect == 0;
    values[cursor++] = memory.AllocationBase == owner;
    check_free(release, info.hProcess, base, 2, MEM_DECOMMIT, owner, 8192);
    values[cursor++] = VirtualAllocEx(info.hProcess, owner, 8192, MEM_COMMIT, PAGE_READWRITE) == owner;
    values[cursor++] = ReadProcessMemory(info.hProcess, owner, &checked, 4, &transferred);
    values[cursor++] = transferred == 4;
    values[cursor++] = checked == 0;
    base = (BYTE *)owner + 1;
    check_free(release, info.hProcess, base, 0, MEM_DECOMMIT, owner, 3 * 4096);
    VirtualQueryEx(info.hProcess, owner, &memory, sizeof(memory));
    values[cursor++] = memory.State == MEM_RESERVE;
    check_free(release, info.hProcess, base, 0, MEM_RELEASE, owner, 3 * 4096);
    VirtualQueryEx(info.hProcess, owner, &memory, sizeof(memory));
    values[cursor++] = memory.State == MEM_FREE;
    check_free(release, info.hProcess, owner, 0, MEM_RELEASE, owner, 0);

    values[cursor++] = ResumeThread(info.hThread) == 1;
    values[cursor++] = WaitForSingleObject(info.hProcess, 5000) == WAIT_OBJECT_0;
    values[cursor++] = GetExitCodeProcess(info.hProcess, &exitcode);
    values[cursor++] = exitcode == 37;
    check_free(release, info.hProcess, owner, 0, MEM_RELEASE, owner, 0);
    values[cursor++] = CloseHandle(restricted);
    values[cursor++] = CloseHandle(info.hThread);
    values[cursor++] = CloseHandle(info.hProcess);
    if (cursor != sizeof(values) / sizeof(values[0])) ExitProcess(3);
    file = CreateFileW(L"nt-remote-free.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, sizeof(values), &written, NULL)
        || written != sizeof(values)) ExitProcess(4);
    CloseHandle(file);
    ExitProcess(0);
}
#endif
