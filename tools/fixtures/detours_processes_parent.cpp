// Owned caller of the actual, unmodified upstream Detours ExA / ExW APIs.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <detours.h>

static DWORD trace[8];
static STARTUPINFOA startup_a;
static STARTUPINFOW startup_w;
static PROCESS_INFORMATION process;
#if defined(WIDE_PROCESS) && defined(SUSPENDED_PROCESS)
static const char trace_name[] = "detours-parent-ws.bin";
static WCHAR command_w[] = L"fixture_detours_child.exe detours-owned-token variant-ws";
#elif defined(WIDE_PROCESS)
static const char trace_name[] = "detours-parent-w.bin";
static WCHAR command_w[] = L"fixture_detours_child.exe detours-owned-token variant-w";
#elif defined(SUSPENDED_PROCESS)
static const char trace_name[] = "detours-parent-as.bin";
static char command_a[] = "fixture_detours_child.exe detours-owned-token variant-as";
#else
static const char trace_name[] = "detours-parent-a.bin";
static char command_a[] = "fixture_detours_child.exe detours-owned-token variant-a";
#endif

extern "C" void mainCRTStartup(void) {
    DWORD flags = 0, written = 0;
    BOOL result;
    HANDLE file;
    trace[0] = 0x44505431;
    trace[1] = 1;
#ifdef WIDE_PROCESS
    trace[2] = 1;
#endif
#ifdef SUSPENDED_PROCESS
    trace[3] = 1;
    flags = CREATE_SUSPENDED;
#endif
    startup_a.cb = sizeof startup_a;
    startup_w.cb = sizeof startup_w;
    SetLastError(0);
#ifdef WIDE_PROCESS
    result = DetourCreateProcessWithDllExW(L"fixture_detours_child.exe", command_w,
        NULL, NULL, FALSE, flags, NULL, NULL, &startup_w, &process,
        "fixture_detours_injected.dll", NULL);
#else
    result = DetourCreateProcessWithDllExA("fixture_detours_child.exe", command_a,
        NULL, NULL, FALSE, flags, NULL, NULL, &startup_a, &process,
        "fixture_detours_injected.dll", NULL);
#endif
    trace[4] = result;
    trace[5] = result ? 0 : GetLastError();
    if (result) {
#ifdef SUSPENDED_PROCESS
        trace[6] = ResumeThread(process.hThread);
#endif
        trace[7] = CloseHandle(process.hThread) && CloseHandle(process.hProcess);
    }
    file = CreateFileA(trace_name, GENERIC_WRITE, FILE_SHARE_READ, NULL,
        CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(110);
    if (!WriteFile(file, trace, sizeof trace, &written, NULL) || written != sizeof trace) ExitProcess(111);
    CloseHandle(file);
    ExitProcess(result ? 0 : 112);
}
