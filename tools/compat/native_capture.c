/* Suspended x86 launcher with LoadLibraryW injection and bounded job lifetime.
 * Usage: native_capture --dll FILE --output FILE --seconds N --max N
 *        [--include GLOBS] [--exclude GLOBS] [--cwd DIR] -- EXE [ARGS...]
 * Output contains only collector/process status, never sample command text.
 */
#include "native_trace.h"
#include <shellapi.h>
#include <tlhelp32.h>

static int same(const WCHAR *a, const WCHAR *b) {
    while (*a && *a == *b) { ++a; ++b; } return *a == *b;
}
static DWORD number(const WCHAR *p) { DWORD n = 0; while (*p >= '0' && *p <= '9') n = n * 10 + *p++ - '0'; return n; }
static void copy_w(WCHAR *to, const WCHAR *from, DWORD capacity) {
    DWORD i = 0; while (from[i] && i + 1 < capacity) { to[i] = from[i]; ++i; } to[i] = 0;
}
static void copy_a(char *to, const WCHAR *from, DWORD capacity) {
    DWORD i = 0; while (from[i] && i + 1 < capacity) { to[i] = (char)from[i]; ++i; } to[i] = 0;
}
static void status(const char *message, DWORD value) {
    char line[256]; DWORD n = 0, written; while (*message) line[n++] = *message++;
    line[n++] = '='; char digits[10]; int i = 0;
    do { digits[i++] = (char)('0' + value % 10); value /= 10; } while (value);
    while (i) line[n++] = digits[--i]; line[n++] = '\n';
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), line, n, &written, NULL);
}
static DWORD remote_base(DWORD pid, const WCHAR *name) {
    HANDLE snapshot = INVALID_HANDLE_VALUE;
    for (int i = 0; i < 10 && snapshot == INVALID_HANDLE_VALUE; ++i)
        snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
    if (snapshot == INVALID_HANDLE_VALUE) return 0;
    MODULEENTRY32W module; module.dwSize = sizeof(module);
    DWORD result = 0;
    if (Module32FirstW(snapshot, &module)) do {
        if (lstrcmpiW(module.szModule, name) == 0) { result = (DWORD)module.modBaseAddr; break; }
    } while (Module32NextW(snapshot, &module));
    CloseHandle(snapshot); return result;
}
static DWORD remote_run(HANDLE process, LPTHREAD_START_ROUTINE function, LPVOID data, DWORD *code) {
    HANDLE thread = CreateRemoteThread(process, NULL, 0, function, data, 0, NULL);
    if (!thread) return GetLastError();
    DWORD wait = WaitForSingleObject(thread, 15000), result = 0;
    if (wait != WAIT_OBJECT_0) result = wait == WAIT_TIMEOUT ? ERROR_TIMEOUT : GetLastError();
    else if (!GetExitCodeThread(thread, code)) result = GetLastError();
    CloseHandle(thread); return result;
}
static DWORD inject(PROCESS_INFORMATION *process, const WCHAR *dll, TraceConfig *config) {
    WCHAR dll_path[1024], owner_path[1024];
    if (!GetFullPathNameW(dll, 1024, dll_path, NULL)) return GetLastError();
    DWORD bytes = (lstrlenW(dll_path) + 1) * sizeof(WCHAR);
    DWORD size = sizeof(*config) > bytes ? sizeof(*config) : bytes;
    LPVOID remote = VirtualAllocEx(process->hProcess, NULL, size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if (!remote) return GetLastError();
    FARPROC load = GetProcAddress(GetModuleHandleW(L"kernel32.dll"), "LoadLibraryW");
    HMODULE owner = NULL;
    GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, (LPCWSTR)load, &owner);
    GetModuleFileNameW(owner, owner_path, 1024);
    WCHAR *base_name = owner_path;
    for (WCHAR *p = owner_path; *p; ++p) if (*p == '\\') base_name = p + 1;
    DWORD base = remote_base(process->dwProcessId, base_name), code = 0, result;
    /* A suspended WoW64 process may not publish its loader list yet. KnownDlls
     * in this same-bitness/session launcher share the system image placement;
     * the first remote thread completes loader startup before calling this API.
     * Never use this fallback for cross-bitness or cross-session injection. */
    if (!base) base = (DWORD)owner;
    if (!WriteProcessMemory(process->hProcess, remote, dll_path, bytes, NULL)) { result = GetLastError(); goto done; }
    result = remote_run(process->hProcess, (LPTHREAD_START_ROUTINE)(base + (DWORD)load - (DWORD)owner), remote, &code);
    if (result || !code) { if (!result) result = ERROR_DLL_INIT_FAILED; goto done; }
    HMODULE local = LoadLibraryExW(dll_path, NULL, DONT_RESOLVE_DLL_REFERENCES);
    if (!local) { result = GetLastError(); goto done; }
    FARPROC initialize = GetProcAddress(local, "TraceInitialize");
    DWORD offset = (DWORD)initialize - (DWORD)local;
    FreeLibrary(local);
    if (!initialize) { result = ERROR_PROC_NOT_FOUND; goto done; }
    if (!WriteProcessMemory(process->hProcess, remote, config, sizeof(*config), NULL)) { result = GetLastError(); goto done; }
    result = remote_run(process->hProcess, (LPTHREAD_START_ROUTINE)(code + offset), remote, &code);
    if (!result && code) { status("collector_error", code); result = ERROR_DLL_INIT_FAILED; }
done:
    /* On timeout the caller terminates its job before remote argument reclamation. */
    if (result != ERROR_TIMEOUT) VirtualFreeEx(process->hProcess, remote, 0, MEM_RELEASE);
    return result;
}
static WCHAR *quote(WCHAR *p, const WCHAR *arg) {
    *p++ = '"';
    while (*arg) {
        int slashes = 0; while (*arg == '\\') { ++slashes; ++arg; }
        int count = (*arg == '"' || !*arg) ? slashes * 2 : slashes;
        while (count--) *p++ = '\\';
        if (*arg == '"') *p++ = '\\';
        if (*arg) *p++ = *arg++;
    }
    *p++ = '"'; return p;
}
void mainCRTStartup(void) {
    int argc = 0, first = 0;
    WCHAR **argv = CommandLineToArgvW(GetCommandLineW(), &argc);
    WCHAR dll[1024] = {0}, cwd[1024] = {0};
    TraceConfig config = {0}; config.magic = CONFIG_MAGIC; config.seconds = 20; config.max_calls = 50000;
    for (int i = 1; i < argc; ++i) {
        if (same(argv[i], L"--")) { first = i + 1; break; }
        if (i + 1 >= argc) { status("invalid_arguments", 1); ExitProcess(2); }
        WCHAR *key = argv[i], *value = argv[++i];
        if (same(key, L"--dll")) copy_w(dll, value, 1024);
        else if (same(key, L"--output")) GetFullPathNameW(value, 1024, config.output, NULL);
        else if (same(key, L"--cwd")) copy_w(cwd, value, 1024);
        else if (same(key, L"--seconds")) config.seconds = number(value);
        else if (same(key, L"--max")) config.max_calls = number(value);
        else if (same(key, L"--include")) copy_a(config.include, value, 2048);
        else if (same(key, L"--exclude")) copy_a(config.exclude, value, 2048);
        else { status("invalid_arguments", 2); ExitProcess(2); }
    }
    if (!first || first >= argc || !dll[0] || !config.output[0] || !config.seconds || config.seconds > 3600) {
        status("invalid_arguments", 3); ExitProcess(2);
    }
    if (!cwd[0]) {
        GetFullPathNameW(argv[first], 1024, cwd, NULL);
        WCHAR *last = NULL;
        for (WCHAR *q = cwd; *q; ++q) if (*q == '\\' || *q == '/') last = q;
        if (last) *last = 0; else cwd[0] = 0;
    }
    WCHAR *command = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, 65536);
    DWORD length = 0;
    for (int i = first; i < argc; ++i) length += lstrlenW(argv[i]) * 2 + 4;
    if (!command || length >= 32760) { status("invalid_arguments", 4); ExitProcess(2); }
    WCHAR *p = command;
    for (int i = first; i < argc; ++i) { if (i != first) *p++ = ' '; p = quote(p, argv[i]); } *p = 0;
    STARTUPINFOW startup = {0}; startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESHOWWINDOW; startup.wShowWindow = SW_HIDE;
    PROCESS_INFORMATION process = {0};
    HANDLE job = CreateJobObjectW(NULL, NULL);
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits = {0};
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if (!job || !SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits, sizeof(limits))) {
        status("job_error", GetLastError()); ExitProcess(3);
    }
    if (!CreateProcessW(argv[first], command, NULL, NULL, FALSE, CREATE_SUSPENDED, NULL, cwd[0] ? cwd : NULL, &startup, &process)) {
        status("create_error", GetLastError()); CloseHandle(job); ExitProcess(3);
    }
    if (!AssignProcessToJobObject(job, process.hProcess)) {
        status("job_error", GetLastError()); TerminateProcess(process.hProcess, 3); ExitProcess(3);
    }
    BOOL wow64 = FALSE; IsWow64Process(process.hProcess, &wow64);
    if (!wow64) { status("unsupported_target", 64); CloseHandle(job); ExitProcess(3); }
    config.main_tid = process.dwThreadId;
    DWORD result = inject(&process, dll, &config);
    if (result) { status("injection_error", result); CloseHandle(job); ExitProcess(4); }
    DWORD capture_start = GetTickCount();
    ResumeThread(process.hThread); status("capture_started", 1);
    DWORD wait = WaitForSingleObject(process.hProcess, (config.seconds + 1) * 1000);
    if (wait == WAIT_OBJECT_0) {
        DWORD exit_code; GetExitCodeProcess(process.hProcess, &exit_code); status("target_exit", exit_code);
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting;
        while (GetTickCount() - capture_start < (config.seconds + 1) * 1000) {
            if (!QueryInformationJobObject(job, JobObjectBasicAccountingInformation, &accounting, sizeof(accounting), NULL) ||
                !accounting.ActiveProcesses) break;
            Sleep(100);
        }
        if (QueryInformationJobObject(job, JobObjectBasicAccountingInformation, &accounting, sizeof(accounting), NULL) && accounting.ActiveProcesses)
            status("untraced_family_deadline", accounting.ActiveProcesses);
    }
    else status("capture_deadline", config.seconds);
    CloseHandle(job); CloseHandle(process.hThread); CloseHandle(process.hProcess);
    ExitProcess(0);
}
