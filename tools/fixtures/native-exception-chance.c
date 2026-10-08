/* Authored x86 first/second-chance oracle, with no CRT or external input.
 * Run as chance-0.exe (second chance) or chance-1.exe (first chance).
 * Output: chance.bin, four scalar DWORDs; native process exit is also evidence.
 * The setup exception supplies a real CONTEXT, avoiding unrelated capture APIs.
 */
#include <windows.h>

typedef LONG (WINAPI *RAISE)(EXCEPTION_RECORD *, CONTEXT *, BOOLEAN);
static RAISE raise_native;
static DWORD first, values[4];
static EXCEPTION_RECORD record_;
static char filename[1024];
#define SETUP_CODE 0xe0421000u
#define TEST_CODE 0xe0421001u

static void emit(DWORD stage) {
    DWORD written;
    HANDLE file;
    values[0] = stage; values[1] = TEST_CODE; values[2] = first; values[3] = 0x12345678;
    file = CreateFileA("chance.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE ||
        !WriteFile(file, values, sizeof(values), &written, NULL) || written != sizeof(values))
        ExitProcess(90);
    CloseHandle(file);
}
static LONG WINAPI top_filter(EXCEPTION_POINTERS *p) {
    (void)p; emit(2); ExitProcess(92);
}
static LONG WINAPI observer(EXCEPTION_POINTERS *p) {
    if (p->ExceptionRecord->ExceptionCode == SETUP_CODE) {
        record_.ExceptionCode = TEST_CODE;
        record_.ExceptionAddress = p->ExceptionRecord->ExceptionAddress;
        record_.NumberParameters = 1;
        record_.ExceptionInformation[0] = 0x12345678;
        p->ContextRecord->ContextFlags = 0x1002f;
        emit(0);
        raise_native(&record_, p->ContextRecord, (BOOLEAN)first);
        emit(3); ExitProcess(93);
    }
    if (p->ExceptionRecord->ExceptionCode != TEST_CODE ||
        p->ExceptionRecord->NumberParameters != 1 ||
        p->ExceptionRecord->ExceptionInformation[0] != 0x12345678) {
        emit(4); ExitProcess(94);
    }
    emit(1); ExitProcess(0);
}
void mainCRTStartup(void) {
    DWORD i, length;
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    length = GetModuleFileNameA(NULL, filename, sizeof(filename));
    for (i = 1; i < length; ++i)
        if (filename[i] == '.' && filename[i-1] == '1') first = 1;
    raise_native = (RAISE)GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtRaiseException");
    if (!raise_native) ExitProcess(91);
    SetUnhandledExceptionFilter(top_filter);
    AddVectoredExceptionHandler(1, observer);
    RaiseException(SETUP_CODE, 0, 0, NULL);
    ExitProcess(95);
}
