/* Build: i686-w64-mingw32-gcc -O2 -o report_stub.exe report_stub.c
 * Build: i686-w64-mingw32-gcc -O2 -DSYSCALL_STUB -o report_syscall_stub.exe report_stub.c */
#include <windows.h>

static LONG CALLBACK handled(EXCEPTION_POINTERS *exception)
{
    return exception->ExceptionRecord->ExceptionCode == 0x80000100
        ? EXCEPTION_CONTINUE_EXECUTION : EXCEPTION_CONTINUE_SEARCH;
}

static LONG CALLBACK fatal(EXCEPTION_POINTERS *exception)
{
    (void)exception;
    return EXCEPTION_EXECUTE_HANDLER;
}

int main(void)
{
    /* Repeated handled raises exercise the same set as the fatal PE stub. */
    ULONG_PTR names[2] = {(ULONG_PTR)"example.dll", (ULONG_PTR)"Missing"};
    PVOID handler = AddVectoredExceptionHandler(1, handled);
    RaiseException(0x80000100, 0, 2, names);
    RaiseException(0x80000100, 0, 2, names);
    RemoveVectoredExceptionHandler(handler);
    SetUnhandledExceptionFilter(fatal);
#ifdef SYSCALL_STUB
    HMODULE module = GetModuleHandleA("ntdll.dll");
    FARPROC stub = GetProcAddress(module, "NtApphelpCacheControl");
#else
    HMODULE module = LoadLibraryA("advapi32.dll");
    FARPROC stub = GetProcAddress(module, "CredProfileLoaded");
#endif
    if (!stub) return 2;
    stub();
    return 3;
}
