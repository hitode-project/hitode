/* Authored x86 live thread context contract. No fixed register values or sample
 * input. CHILD provides a live remote owner; output contains scalar outcomes. */
#include <windows.h>

typedef LONG (WINAPI *GET_CONTEXT)(HANDLE, CONTEXT *);

#ifdef CHILD
void mainCRTStartup(void) { Sleep(1000); ExitProcess(37); }
#else
static DWORD values[128], cursor;
static volatile DWORD ticks, stopping;
static HANDLE ready;
static GET_CONTEXT nt_get;

static DWORD WINAPI worker(void *unused) {
    SetEvent(ready);
    while (!stopping) ticks++;
    return 21;
}
static void fill(void *pointer, unsigned length) {
    BYTE *bytes = pointer;
    for (unsigned i = 0; i < length; i++) bytes[i] = 0xa5;
}
static int unchanged(const void *pointer, unsigned length) {
    const BYTE *bytes = pointer;
    for (unsigned i = 0; i < length; i++) if (bytes[i] != 0xa5) return 0;
    return 1;
}
static void append(DWORD value) { values[cursor++] = value; }
static void probe(HANDLE thread, DWORD flags, int win32, int local) {
    CONTEXT context;
    DWORD previous, code, before;
    fill(&context, sizeof(context)); context.ContextFlags = flags;
    SetLastError(0x13572468);
    append(win32 ? GetThreadContext(thread, &context) != 0 : (DWORD)nt_get(thread, &context));
    append(GetLastError()); append(context.ContextFlags);
    BYTE *bytes = (BYTE *)&context;
    append((!(flags & 1) || !unchanged(bytes + 0xb4, 24))
        && (!(flags & 2) || !unchanged(bytes + 0x9c, 24))
        && (!(flags & 4) || !unchanged(bytes + 0x8c, 16)));
    append(unchanged(bytes + 4, 24) && unchanged(bytes + 0x1c, 112)
        && unchanged(bytes + 0xcc, sizeof(context) - 0xcc)
        && ((flags & 1) || unchanged(bytes + 0xb4, 24))
        && ((flags & 2) || unchanged(bytes + 0x9c, 24))
        && ((flags & 4) || unchanged(bytes + 0x8c, 16)));
    previous = SuspendThread(thread); append(previous);
    if (previous == (DWORD)-1) ExitProcess(102);
    append(ResumeThread(thread));
    if (local) {
        before = ticks;
        for (unsigned i = 0; i < 1000 && ticks == before; i++) Sleep(1);
        append(ticks != before);
    } else append(GetExitCodeThread(thread, &code) && code == STILL_ACTIVE);
}
void mainCRTStartup(void) {
    DWORD written, code;
    HANDLE thread, file;
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION child;
    nt_get = (GET_CONTEXT)GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtGetContextThread");
    ready = CreateEventW(NULL, TRUE, FALSE, NULL);
    thread = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    if (!nt_get || !ready || !thread || WaitForSingleObject(ready, 5000)) ExitProcess(101);
    probe(thread, CONTEXT_FULL, 0, 1);
    probe(thread, CONTEXT_FULL, 1, 1);
    probe(thread, CONTEXT_CONTROL, 0, 1);
    probe(thread, CONTEXT_INTEGER, 1, 1);
    if (!CreateProcessW(L"live-context-child.exe", NULL, NULL, NULL, FALSE, CREATE_NO_WINDOW,
            NULL, NULL, &startup, &child)) ExitProcess(103);
    probe(child.hThread, CONTEXT_FULL, 0, 0);
    probe(child.hThread, CONTEXT_FULL, 1, 0);
    stopping = 1;
    append(WaitForSingleObject(thread, 5000));
    append(GetExitCodeThread(thread, &code)); append(code);
    append(CloseHandle(thread)); append(CloseHandle(ready));
    append(WaitForSingleObject(child.hThread, 5000));
    append(GetExitCodeThread(child.hThread, &code)); append(code);
    append(CloseHandle(child.hThread)); append(CloseHandle(child.hProcess));
    file = CreateFileW(L"live-thread-context.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, cursor * 4, &written, NULL)
            || written != cursor * 4) ExitProcess(104);
    CloseHandle(file); ExitProcess(0);
}
#endif
