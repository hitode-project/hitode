/* x86 completed-call collector. Initialize outside DllMain via a remote thread.
 * Input: TraceConfig. Output: local TSV with logical threads and entry sequence.
 * No guest code, instruction addresses or pointer values are written.
 */
#include "native_trace.h"
#include <intrin.h>
#include <tlhelp32.h>
#include "MinHook.h"

typedef struct { unsigned char kind, offset, words; } Param;
typedef struct {
    const char *module, *name;
    int kind;
    const Param *params;
    int count;
    LPVOID detour;
    LPVOID *real;
    LPVOID target;
    int enabled;
    int child_info;
    int result_bits;
} Hook;
typedef struct { DWORD busy, thread, sequence; } Thread;
typedef struct { int hook; DWORD thread, sequence; char args[2048]; } Call;
typedef struct { DWORD start, end; } Range;

static DWORD tls_slot, thread_count = 1, start_tick, accepted;
static HANDLE output_file;
static CRITICAL_SECTION output_lock;
static char buffer[65536];
static DWORD used;
static volatile LONG stopped;
static TraceConfig config;
static HMODULE collector_module;
static Range system_ranges[128];
static int range_count;
static LPVOID (WINAPI *tls_get)(DWORD) = TlsGetValue;
static BOOL (WINAPI *tls_set)(DWORD, LPVOID) = TlsSetValue;
static DWORD last_error(void) { return __readfsdword(0x34); }
static void set_error(DWORD value) {
    __asm {
        mov eax, value
        mov fs:[0x34], eax
    }
}
static int begin_call(Call *, int, const DWORD *, void *);
static void end_call(Call *, const DWORD *, unsigned long long);

#include "hooks.inc"

static char *put(char *p, const char *s) { while (*s) *p++ = *s++; return p; }
static char *hex(char *p, unsigned long long n) {
    char digits[16]; int i = 0;
    do { digits[i++] = "0123456789abcdef"[n & 15]; n >>= 4; } while (n);
    while (i) *p++ = digits[--i];
    return p;
}
static char *decimal(char *p, DWORD n) {
    char digits[10]; int i = 0;
    do { digits[i++] = (char)('0' + n % 10); n /= 10; } while (n);
    while (i) *p++ = digits[--i];
    return p;
}
static int equal(const char *a, const char *b) {
    while (*a && *a == *b) { ++a; ++b; }
    return *a == *b;
}
static int window_word(const Hook *hook, const DWORD *raw) {
    const char *names[] = {"GetWindowLongA", "GetWindowLongW", "GetWindowLongPtrA", "GetWindowLongPtrW",
        "SetWindowLongA", "SetWindowLongW", "SetWindowLongPtrA", "SetWindowLongPtrW",
        "GetClassLongA", "GetClassLongW", "GetClassLongPtrA", "GetClassLongPtrW",
        "SetClassLongA", "SetClassLongW", "SetClassLongPtrA", "SetClassLongPtrW"};
    for (int i = 0; i < sizeof(names)/sizeof(names[0]); ++i) if (equal(hook->name, names[i])) {
        LONG index = (LONG)raw[1];
        return i < 8 ? index != -12 && index != -16 && index != -20 :
            index != -26 && index != -18 && index != -20 && index != -32;
    }
    return 0;
}
static int glob(const char *pattern, const char *end, const char *value) {
    const char *star = NULL, *retry = NULL;
    while (*value) {
        if (pattern < end && (*pattern == '?' || *pattern == *value)) { ++pattern; ++value; }
        else if (pattern < end && *pattern == '*') { star = pattern++; retry = value; }
        else if (star) { pattern = star + 1; value = ++retry; }
        else return 0;
    }
    while (pattern < end && *pattern == '*') ++pattern;
    return pattern == end;
}
static int matches(const char *patterns, const char *value) {
    while (*patterns) {
        const char *end = patterns;
        while (*end && *end != ';') ++end;
        if (glob(patterns, end, value)) return 1;
        patterns = *end ? end + 1 : end;
    }
    return 0;
}
static void flush_locked(void) {
    DWORD done = 0, offset = 0;
    while (offset < used) {
        if (!WriteFile(output_file, buffer + offset, used - offset, &done, NULL) || !done) {
            InterlockedExchange(&stopped, 1); break;
        }
        offset += done;
    }
    used = 0;
}
static void write_line(const char *line, DWORD length) {
    EnterCriticalSection(&output_lock);
    if (used + length > sizeof(buffer)) flush_locked();
    if (length <= sizeof(buffer)) {
        for (DWORD i = 0; i < length; ++i) buffer[used++] = line[i];
    }
    LeaveCriticalSection(&output_lock);
}
static void note(const char *label, const char *api, DWORD value) {
    char line[512], *p = put(line, "# ");
    p = put(p, label); *p++ = '\t'; p = put(p, api); *p++ = '\t';
    p = hex(p, value); *p++ = '\n'; write_line(line, (DWORD)(p - line));
}
static Thread *thread_state(void) {
    Thread *state = (Thread *)tls_get(tls_slot);
    if (state == (Thread *)1) return NULL;
    if (!state) {
        tls_set(tls_slot, (LPVOID)1);
        state = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(Thread));
        if (!state) return NULL;
        state->thread = GetCurrentThreadId() == config.main_tid ? 1 : InterlockedIncrement((LONG *)&thread_count);
        tls_set(tls_slot, state);
    }
    return state;
}
static char *format_value(char *p, int kind, unsigned long long value) {
    if (kind == 0) return put(p, "void");
    if (kind == 4 || kind == 10) return put(p, value ? "ptr" : "null");
    if (kind == 3) { p = put(p, "h:"); return hex(p, value); }
    if (kind == 5 || kind == 6) {
        if (!value) return put(p, "null");
        if (value <= 0xffff) { p = put(p, "atom:"); return hex(p, value); }
        p = put(p, kind == 5 ? "s8:" : "s16:");
        char *start = p;
        MEMORY_BASIC_INFORMATION info; BYTE text[256]; SIZE_T copied = 0;
        if (!VirtualQuery((LPCVOID)(DWORD)value, &info, sizeof(info))) return put(start, "unreadable");
        DWORD available = (DWORD)info.BaseAddress + info.RegionSize - (DWORD)value;
        DWORD bytes = kind == 5 ? 128 : 256;
        if (available < bytes) bytes = available;
        if (!ReadProcessMemory((HANDLE)-1, (LPCVOID)(DWORD)value, text, bytes, &copied)) return put(start, "unreadable");
        int limit = (int)copied / (kind == 5 ? 1 : 2);
        for (int i = 0; i < limit; ++i) {
            unsigned int unit = kind == 5 ? text[i] : ((WORD *)text)[i];
            if (!unit) return p;
            for (int shift = kind == 5 ? 4 : 12; shift >= 0; shift -= 4)
                *p++ = "0123456789abcdef"[(unit >> shift) & 15];
        }
        return put(p, "~");
    }
    return hex(p, value);
}
static int begin_call(Call *call, int index, const DWORD *raw, void *caller) {
    DWORD address = (DWORD)caller;
    Thread *state;
    if (stopped || !hooks[index].enabled) return 0;
    for (int i = 0; i < range_count; ++i)
        if (address >= system_ranges[i].start && address < system_ranges[i].end) return 0;
    state = thread_state();
    if (!state || state->busy) return 0;
    state->busy = 1;
    DWORD number = InterlockedIncrement((LONG *)&accepted);
    if ((config.max_calls && number > config.max_calls) ||
        (config.seconds && GetTickCount() - start_tick >= config.seconds * 1000)) {
        if (!InterlockedExchange(&stopped, 1)) note("capture_stop",
            config.max_calls && number > config.max_calls ? "limit" : "seconds", number - 1);
        state->busy = 0; return 0;
    }
    call->hook = index; call->thread = state->thread; call->sequence = ++state->sequence;
    Hook *hook = &hooks[index]; char *p = call->args;
    for (int i = 0; i < hook->count; ++i) {
        Param param = hook->params[i];
        unsigned long long value = raw[param.offset];
        if (param.words == 2) value |= (unsigned long long)raw[param.offset + 1] << 32;
        if (i) *p++ = ',';
        /* The bounded per-call buffer leaves room for the remaining scalar slots. */
        if (p - call->args > 1400) { p = put(p, "omitted"); break; }
        p = format_value(p, i == 2 && window_word(hook, raw) ? 4 : param.kind, value);
    }
    *p = 0;
    if (equal(hook->name, "ExitProcess") || equal(hook->name, "ExitThread")) {
        char line[3072], *q = put(line, "# terminal\texit\t");
        q = decimal(q, call->thread); q = put(q, "\t0\t");
        q = put(q, hook->module); *q++ = '!'; q = put(q, hook->name);
        *q++ = '\t'; q = put(q, call->args); q = put(q, "\t0\n");
        write_line(line, (DWORD)(q - line));
        EnterCriticalSection(&output_lock); flush_locked(); LeaveCriticalSection(&output_lock);
        if (equal(hook->name, "ExitProcess")) InterlockedExchange(&stopped, 1);
        /* Suppress thread detach activity after a nonreturning exit. */
        return 1;
    }
    state->busy = 0;
    return 1;
}
static void end_call(Call *call, const DWORD *raw, unsigned long long value) {
    Thread *state = thread_state();
    if (!state) return;
    state->busy = 1;
    Hook *hook = &hooks[call->hook];
    if (hook->result_bits < 64) value &= (1ULL << hook->result_bits) - 1;
    char line[4096], *p = decimal(line, call->thread);
    p = put(p, "\t0\t"); p = put(p, hook->module); *p++ = '!'; p = put(p, hook->name);
    *p++ = '\t'; p = put(p, call->args); *p++ = '\t';
    /* Returned strings are addresses in the host trace, so retain only their category. */
    p = format_value(p, hook->kind == 5 || hook->kind == 6 || window_word(hook, raw) ? 4 : hook->kind, value);
    *p++ = '\t'; p = decimal(p, call->sequence); *p++ = '\n';
    write_line(line, (DWORD)(p - line));
    if (hook->child_info >= 0 && value) {
        PROCESS_INFORMATION info;
        if (ReadProcessMemory((HANDLE)-1, (LPCVOID)raw[hook->child_info], &info, sizeof(info), NULL))
            note("child_untraced", hook->name, info.dwProcessId);
        else note("child_untraced", hook->name, 0);
    }
    state->busy = 0;
}
static DWORD WINAPI flush_worker(LPVOID unused) {
    (void)unused; tls_set(tls_slot, (LPVOID)1);
    for (;;) {
        Sleep(250);
        EnterCriticalSection(&output_lock); flush_locked(); LeaveCriticalSection(&output_lock);
        if (config.seconds && GetTickCount() - start_tick >= config.seconds * 1000) {
            if (!InterlockedExchange(&stopped, 1)) note("capture_stop", "seconds", accepted);
            EnterCriticalSection(&output_lock); flush_locked(); LeaveCriticalSection(&output_lock);
            return 0;
        }
    }
}
static void add_range(HMODULE module) {
    if (!module || range_count == 128) return;
    DWORD start = (DWORD)module;
    for (int i = 0; i < range_count; ++i) if (system_ranges[i].start == start) return;
    IMAGE_DOS_HEADER *dos = (IMAGE_DOS_HEADER *)module;
    IMAGE_NT_HEADERS *nt = (IMAGE_NT_HEADERS *)(start + dos->e_lfanew);
    system_ranges[range_count].start = start;
    system_ranges[range_count++].end = start + nt->OptionalHeader.SizeOfImage;
}
static void system_module_ranges(void) {
    WCHAR windows[1024]; UINT length = GetWindowsDirectoryW(windows, 1024);
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
    if (snapshot == INVALID_HANDLE_VALUE || !length || length >= 1024) return;
    MODULEENTRY32W module; module.dwSize = sizeof(module);
    if (Module32FirstW(snapshot, &module)) do {
        if (CompareStringOrdinal(windows, length, module.szExePath, length, TRUE) == CSTR_EQUAL &&
            (module.szExePath[length] == '\\' || module.szExePath[length] == '/')) add_range(module.hModule);
    } while (Module32NextW(snapshot, &module));
    CloseHandle(snapshot);
}
__declspec(dllexport) DWORD WINAPI TraceInitialize(LPVOID input) {
    if (output_file || !input || ((TraceConfig *)input)->magic != CONFIG_MAGIC) return 1;
    config = *(TraceConfig *)input;
    tls_slot = TlsAlloc();
    if (tls_slot == TLS_OUT_OF_INDEXES) return 2;
    tls_set(tls_slot, (LPVOID)1);
    InitializeCriticalSection(&output_lock);
    output_file = CreateFileW(config.output, GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (output_file == INVALID_HANDLE_VALUE) { output_file = NULL; return 3; }
    const char header[] = "# native_api_trace\tversion=1\tsequence=entry\tchildren=untraced\n";
    write_line(header, sizeof(header) - 1);
    const char *modules[] = {"kernel32.dll", "kernelbase.dll", "ntdll.dll", "user32.dll", "gdi32.dll", "advapi32.dll", "winmm.dll", "ole32.dll", "msvcrt.dll", "ucrtbase.dll", "combase.dll", "win32u.dll"};
    for (int i = 0; i < sizeof(modules)/sizeof(modules[0]); ++i) add_range(LoadLibraryA(modules[i]));
    add_range(collector_module);
    system_module_ranges();
    if (MH_Initialize() != MH_OK) return 4;
    DWORD installed = 0, failed = 0;
    for (int i = 0; i < sizeof(hooks)/sizeof(hooks[0]); ++i) {
        Hook *hook = &hooks[i]; char api[256], *p = put(api, hook->module);
        *p++ = '!'; p = put(p, hook->name); *p = 0;
        if ((config.include[0] && !matches(config.include, api)) || matches(config.exclude, api)) continue;
        hook->target = (LPVOID)GetProcAddress(GetModuleHandleA(hook->module), hook->name);
        if (!hook->target) { note("hook_missing", api, 0); ++failed; continue; }
        MH_STATUS status = MH_CreateHook(hook->target, hook->detour, hook->real);
        if (status != MH_OK) { note("hook_failed", api, status); ++failed; continue; }
        if (equal(hook->name, "TlsGetValue")) tls_get = (LPVOID (WINAPI *)(DWORD))*hook->real;
        if (equal(hook->name, "TlsSetValue")) tls_set = (BOOL (WINAPI *)(DWORD, LPVOID))*hook->real;
        hook->enabled = 1; ++installed;
        note("hook", api, 0);
    }
    start_tick = GetTickCount();
    note("hooks_installed", "count", installed); note("hooks_unavailable", "count", failed);
    EnterCriticalSection(&output_lock); flush_locked(); LeaveCriticalSection(&output_lock);
    if (MH_EnableHook(MH_ALL_HOOKS) != MH_OK) return 5;
    HANDLE worker = CreateThread(NULL, 0, flush_worker, NULL, 0, NULL);
    if (worker) CloseHandle(worker);
    return worker ? 0 : 6;
}
BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) { collector_module = instance; DisableThreadLibraryCalls(instance); }
    if (reason == DLL_PROCESS_DETACH && output_file) {
        /* Do not acquire a lock held by another thread during process termination. */
        if (TryEnterCriticalSection(&output_lock)) { flush_locked(); LeaveCriticalSection(&output_lock); }
    }
    return TRUE;
}
