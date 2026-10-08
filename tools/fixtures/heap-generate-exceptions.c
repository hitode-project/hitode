/* Authored x86 native heap exception oracle; no samples or runtime code inspection.
   Build with clang-cl targeting i686, /GS- /Zl and link kernel32.lib /nodefaultlib.
   Usage: heap-generate-exceptions.exe CASE (0..19), or heap-CASE.exe without an
   argument. Four cases per HeapAlloc,
   RtlAllocateHeap, HeapReAlloc, RtlReAllocateHeap: success with 0xc; impossible
   allocation with 0, 4, 0xc. Stdout is one JSON row containing only contract data.
   Cases 16..19 continue the flags=4 failure through a vectored handler and
   observe the API return. Other exception cases exit from the observer, so
   even noncontinuable exceptions do not require CRT handlers. */
#include <windows.h>

typedef PVOID (WINAPI *ALLOCATE)(PVOID, ULONG, SIZE_T);
typedef PVOID (WINAPI *REALLOCATE)(PVOID, ULONG, PVOID, SIZE_T);
static const char *names[4] = {"HeapAlloc", "RtlAllocateHeap", "HeapReAlloc", "RtlReAllocateHeap"};
static HANDLE heap;
static PVOID original, result;
static DWORD api, mode, heap_flags, request, raised_code, raised_flags, parameters, argument0;
static DWORD error, original_ok = 1, zero_ok = 1;
static char output[768];
static char executable[260];
static DWORD used;
static void text(const char *s) { while (*s) output[used++] = *s++; }
static void number(DWORD value) {
    char digits[10]; DWORD count = 0;
    do { digits[count++] = (char)('0' + value % 10); value /= 10; } while (value);
    while (count) output[used++] = digits[--count];
}
static void field(const char *key, DWORD value) { text(",\""); text(key); text("\":"); number(value); }
static void emit(void) {
    DWORD written; HANDLE file;
    text("{\"api\":\""); text(names[api]); text("\"");
    field("mode", mode); field("heap_flags", heap_flags); field("request", request);
    field("result", result != NULL); field("exception_code", raised_code);
    field("exception_flags", raised_flags); field("exception_parameters", parameters);
    field("argument0", argument0); field("original_ok", original_ok); field("zero_ok", zero_ok);
    field("last_error", error); text("}\r\n");
    if (!WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), output, used, &written, NULL) || written != used) ExitProcess(90);
    file = CreateFileA("heap-generate.json", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, output, used, &written, NULL) || written != used) ExitProcess(94);
    CloseHandle(file);
}
static void check_original(void) {
    DWORD i;
    if (!original) return;
    original_ok = HeapSize(heap, 0, original) == 16;
    for (i = 0; i < 16; ++i) original_ok = original_ok && ((BYTE *)original)[i] == 0x3c;
}
static LONG WINAPI observer(EXCEPTION_POINTERS *pointers) {
    EXCEPTION_RECORD *record = pointers->ExceptionRecord;
    raised_code = record->ExceptionCode; raised_flags = record->ExceptionFlags;
    parameters = record->NumberParameters;
    argument0 = parameters ? (DWORD)record->ExceptionInformation[0] : 0;
    error = GetLastError();
    if (mode == 4) return EXCEPTION_CONTINUE_EXECUTION;
    check_original(); emit(); ExitProcess(0);
}
void mainCRTStartup(void) {
    DWORD which = 0, i;
    char *last, *name;
    HMODULE nt = GetModuleHandleW(L"ntdll.dll");
    ALLOCATE allocate = (ALLOCATE)GetProcAddress(nt, "RtlAllocateHeap");
    REALLOCATE reallocate = (REALLOCATE)GetProcAddress(nt, "RtlReAllocateHeap");
    if (!GetModuleFileNameA(NULL, executable, sizeof executable)) ExitProcess(95);
    name = last = executable;
    while (*name) { if (*name == '\\' || *name == '/') last = name + 1; ++name; }
    if (last[0] == 'h' && last[1] == 'e' && last[2] == 'a' && last[3] == 'p' && last[4] == '-') last += 5;
    else {
        char *command = GetCommandLineA(); last = command;
        while (*command) { if (*command == ' ') last = command + 1; ++command; }
    }
    while (*last >= '0' && *last <= '9') which = which * 10 + (*last++ - '0');
    if (which > 19 || !allocate || !reallocate) ExitProcess(91);
    api = which < 16 ? which / 4 : which - 16;
    mode = which < 16 ? which % 4 : 4;
    heap_flags = mode == 1 ? 0 : mode == 2 || mode == 4 ? 4 : 12;
    request = mode == 0 ? 64 : 0xffffffff;
    heap = HeapCreate(0, 4096, 65536);
    if (!heap || !AddVectoredExceptionHandler(1, observer)) ExitProcess(92);
    if (api >= 2) {
        original = HeapAlloc(heap, 0, 16);
        if (!original) ExitProcess(93);
        for (i = 0; i < 16; ++i) ((BYTE *)original)[i] = 0x3c;
    }
    SetLastError(0x12345678);
    switch (api) {
        case 0: result = HeapAlloc(heap, heap_flags, request); break;
        case 1: result = allocate(heap, heap_flags, request); break;
        case 2: result = HeapReAlloc(heap, heap_flags, original, request); break;
        case 3: result = reallocate(heap, heap_flags, original, request); break;
    }
    error = GetLastError();
    if (result) {
        for (i = api >= 2 ? 16 : 0; i < request; ++i) zero_ok = zero_ok && ((BYTE *)result)[i] == 0;
        if (api >= 2) for (i = 0; i < 16; ++i) original_ok = original_ok && ((BYTE *)result)[i] == 0x3c;
    } else check_original();
    emit();
    if (result) HeapFree(heap, 0, result); else if (original) HeapFree(heap, 0, original);
    HeapDestroy(heap); ExitProcess(0);
}
