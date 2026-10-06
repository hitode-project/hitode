/* Owned x86 process-family differential fixture; no CRT or third-party code.
 * Parent trace: 64 little-endian DWORDs (documented at each assignment).
 * Child traces: 8 DWORDs; first waits/returns 37, late writes after parent exit.
 * Parent and child retain identical sections by compiling all functions /Od;
 * only this fixed-size role string differs between the two builds. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#ifdef CHILD
static volatile char fixture_role[7] = "CHILD ";
#else
static volatile char fixture_role[7] = "PARENT";
#endif
__declspec(dllexport) volatile DWORD isolation_word = 0x11223344;
static DWORD trace[64];
static char directory[MAX_PATH];
static char environment[1024];
static char command_a[] = "fixture_guest_child.exe first owned-token";
static WCHAR command_w[] = L"fixture_guest_child.exe late owned-token";
static CONTEXT before_context, changed_context, checked_context;

/* Clang can lower aggregate copies into these routines even without the CRT. */
void *memset(void *pointer, int value, unsigned int size) {
    volatile BYTE *bytes = (volatile BYTE *)pointer;
    unsigned int index;
    for (index = 0; index < size; index++) bytes[index] = (BYTE)value;
    return pointer;
}
void *memcpy(void *destination, const void *source, unsigned int size) {
    volatile BYTE *out = (volatile BYTE *)destination;
    const volatile BYTE *in = (const volatile BYTE *)source;
    unsigned int index;
    for (index = 0; index < size; index++) out[index] = in[index];
    return destination;
}
static DWORD length(const char *value) {
    DWORD result = 0;
    while (value[result]) result++;
    return result;
}
static BOOL equal(const char *left, const char *right) {
    DWORD index = 0;
    while (left[index] && left[index] == right[index]) index++;
    return left[index] == right[index];
}
static BOOL contains(const char *value, const char *needle) {
    DWORD at, index;
    for (at = 0; value[at]; at++) {
        for (index = 0; needle[index] && value[at + index] == needle[index]; index++);
        if (!needle[index]) return TRUE;
    }
    return FALSE;
}
static BOOL write_trace(const char *name, const DWORD *words, DWORD count) {
    HANDLE file = CreateFileA(name, GENERIC_WRITE, FILE_SHARE_READ, NULL,
        CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    DWORD written = 0;
    BOOL result;
    if (file == INVALID_HANDLE_VALUE) return FALSE;
    result = WriteFile(file, words, count * sizeof(DWORD), &written, NULL);
    CloseHandle(file);
    return result && written == count * sizeof(DWORD);
}
static void make_environment(void) {
    const char marker[] = "GUEST_PROCESS_MARKER=owned";
    const char cwd_key[] = "GUEST_PROCESS_CWD=";
    DWORD offset = 0, index;
    for (index = 0; index < sizeof marker; index++) environment[offset++] = marker[index];
    for (index = 0; index + 1 < sizeof cwd_key; index++) environment[offset++] = cwd_key[index];
    for (index = 0; directory[index]; index++) environment[offset++] = directory[index];
    environment[offset++] = 0;
    environment[offset] = 0;
}
static void child_main(void) {
    DWORD output[8];
    char marker[32], expected[MAX_PATH], cwd[MAX_PATH];
    BOOL late = contains(GetCommandLineA(), " late ");
    output[0] = 0x47504331;
    output[1] = 1;
    output[2] = late ? 2 : 1;
    output[3] = isolation_word;
    output[4] = contains(GetCommandLineA(), "owned-token");
    GetEnvironmentVariableA("GUEST_PROCESS_MARKER", marker, sizeof marker);
    output[5] = equal(marker, "owned");
    GetEnvironmentVariableA("GUEST_PROCESS_CWD", expected, sizeof expected);
    GetCurrentDirectoryA(sizeof cwd, cwd);
    output[6] = equal(cwd, expected);
    output[7] = (DWORD)GetModuleHandleA(NULL);
    /* The second phase deliberately cannot write before the parent has exited. */
    if (late) Sleep(150);
    if (!write_trace(late ? "process-child-late.bin" : "process-child.bin", output, 8)) ExitProcess(101);
    ExitProcess(37);
}
static void parent_main(void) {
    STARTUPINFOA startup_a;
    STARTUPINFOW startup_w;
    PROCESS_INFORMATION first, late;
    MEMORY_BASIC_INFORMATION info;
    HANDLE duplicate = NULL, limited = NULL;
    DWORD code = 0, value = 0, old = 0, transferred = 0;
    void *allocation = NULL;
    BOOL created;
    memset(&startup_a, 0, sizeof startup_a);
    memset(&startup_w, 0, sizeof startup_w);
    memset(&first, 0, sizeof first);
    memset(&late, 0, sizeof late);
    startup_a.cb = sizeof startup_a;
    startup_w.cb = sizeof startup_w;
    GetCurrentDirectoryA(sizeof directory, directory);
    make_environment();
    isolation_word = 0xaabbccdd;
    trace[0] = 0x47505031;
    trace[1] = 1;
    created = CreateProcessA("fixture_guest_child.exe", command_a, NULL, NULL, TRUE,
        CREATE_SUSPENDED, environment, directory, &startup_a, &first);
    trace[2] = created;
    if (!created) { trace[3] = GetLastError(); write_trace("process-parent.bin", trace, 64); ExitProcess(102); }
    trace[3] = first.dwProcessId != GetCurrentProcessId();
    trace[4] = first.dwThreadId != GetCurrentThreadId();
    trace[5] = GetExitCodeProcess(first.hProcess, &code);
    trace[6] = code; /* STILL_ACTIVE=259 */
    trace[7] = GetExitCodeThread(first.hThread, &code);
    trace[8] = code;
    trace[9] = WaitForSingleObject(first.hProcess, 0); /* WAIT_TIMEOUT=258 */
    trace[10] = ReadProcessMemory(first.hProcess, (const void *)&isolation_word, &value, sizeof value, &transferred);
    trace[11] = transferred;
    trace[12] = value; /* Child's untouched initial image global. */
    value = 0x55667788;
    trace[13] = WriteProcessMemory(first.hProcess, (void *)&isolation_word, &value, sizeof value, &transferred);
    trace[14] = transferred;
    trace[15] = isolation_word; /* Parent remains 0xaabbccdd. */
    trace[16] = ReadProcessMemory(first.hProcess, (const void *)&isolation_word, &value, sizeof value, &transferred);
    trace[17] = value;
    allocation = VirtualAllocEx(first.hProcess, NULL, 0x2000, MEM_RESERVE, PAGE_READWRITE);
    trace[18] = allocation != NULL;
    trace[19] = VirtualQueryEx(first.hProcess, allocation, &info, sizeof info);
    trace[20] = info.State; /* MEM_RESERVE=0x2000 */
    trace[21] = VirtualAllocEx(first.hProcess, allocation, 0x1000, MEM_COMMIT, PAGE_READWRITE) == allocation;
    trace[22] = VirtualQueryEx(first.hProcess, allocation, &info, sizeof info);
    trace[23] = info.State; /* MEM_COMMIT=0x1000 */
    trace[24] = info.Protect; /* PAGE_READWRITE=4 */
    trace[25] = VirtualProtectEx(first.hProcess, allocation, 0x1000, PAGE_READONLY, &old);
    trace[26] = old;
    trace[27] = VirtualQueryEx(first.hProcess, allocation, &info, sizeof info);
    trace[28] = info.Protect; /* PAGE_READONLY=2 */
    trace[29] = VirtualFreeEx(first.hProcess, allocation, 0, MEM_RELEASE);
    trace[30] = SuspendThread(first.hThread); /* Initial count=1 -> 2. */
    trace[31] = ResumeThread(first.hThread); /* 2 -> 1; remains suspended. */
    before_context.ContextFlags = CONTEXT_INTEGER;
    changed_context.ContextFlags = CONTEXT_INTEGER;
    checked_context.ContextFlags = CONTEXT_INTEGER;
    trace[32] = GetThreadContext(first.hThread, &before_context);
    memcpy(&changed_context, &before_context, sizeof changed_context);
    changed_context.Ebx = 0x87654321;
    trace[33] = SetThreadContext(first.hThread, &changed_context);
    trace[34] = GetThreadContext(first.hThread, &checked_context);
    trace[35] = checked_context.Ebx == 0x87654321;
    trace[36] = SetThreadContext(first.hThread, &before_context);
    trace[37] = DuplicateHandle(GetCurrentProcess(), first.hProcess, GetCurrentProcess(), &duplicate, 0, FALSE, DUPLICATE_SAME_ACCESS);
    limited = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, first.dwProcessId);
    trace[38] = limited != NULL;
    trace[39] = GetExitCodeProcess(limited, &code);
    trace[40] = code;
    if (limited) CloseHandle(limited);
    SetLastError(0);
    trace[41] = GetExitCodeProcess((HANDLE)0xdeadbeef, &code);
    trace[42] = GetLastError(); /* ERROR_INVALID_HANDLE=6 */
    SetLastError(0);
    trace[43] = ReadProcessMemory((HANDLE)0xdeadbeef, (const void *)&isolation_word, &value, sizeof value, &transferred);
    trace[44] = GetLastError();
    SetLastError(0);
    trace[45] = SuspendThread((HANDLE)0xdeadbeef);
    trace[46] = GetLastError();
    trace[47] = ResumeThread(first.hThread); /* 1 -> 0. */
    trace[48] = WaitForSingleObject(duplicate, INFINITE); /* WAIT_OBJECT_0=0 */
    trace[49] = GetExitCodeProcess(duplicate, &code);
    trace[50] = code; /* 37 */
    trace[51] = WaitForSingleObject(first.hThread, 0);
    trace[52] = GetExitCodeThread(first.hThread, &code);
    trace[53] = code;
    trace[54] = CloseHandle(first.hThread);
    trace[55] = CloseHandle(first.hProcess);
    trace[56] = CloseHandle(duplicate);
    /* lpDirectory is NULL: inherit the parent's current directory for W creation. */
    trace[57] = CreateProcessW(L"fixture_guest_child.exe", command_w, NULL, NULL, FALSE,
        CREATE_SUSPENDED, environment, NULL, &startup_w, &late);
    if (trace[57]) {
        trace[58] = ResumeThread(late.hThread);
        trace[59] = CloseHandle(late.hThread);
        trace[60] = CloseHandle(late.hProcess);
    }
    trace[61] = (DWORD)GetModuleHandleA(NULL);
    trace[62] = sizeof trace;
    trace[63] = 0x454e4431;
    if (!write_trace("process-parent.bin", trace, 64)) ExitProcess(103);
    ExitProcess(0);
}
void mainCRTStartup(void) {
    if (fixture_role[0] == 'C') child_main();
    else parent_main();
}
