/* Build: i686-w64-mingw32-gcc -O2 -o report_child_fault.exe report_child_fault.c */
#include <windows.h>
#include <stdio.h>

static LONG CALLBACK fatal(EXCEPTION_POINTERS *exception)
{
    typedef LONG (WINAPI *Raise)(EXCEPTION_RECORD *, CONTEXT *, BOOL);
    Raise raise = (Raise)GetProcAddress(GetModuleHandleA("ntdll.dll"), "NtRaiseException");
    raise(exception->ExceptionRecord, exception->ContextRecord, FALSE);
    return EXCEPTION_CONTINUE_SEARCH;
}

int main(int argc, char **argv)
{
    (void)argv;
    if (argc > 1) {
        SetUnhandledExceptionFilter(fatal);
        __asm__ volatile("ud2");
        return 3;
    }
    char exe[MAX_PATH], command[MAX_PATH + 16];
    GetModuleFileNameA(NULL, exe, sizeof(exe));
    snprintf(command, sizeof(command), "\"%s\" child", exe);
    STARTUPINFOA startup = {0};
    PROCESS_INFORMATION process;
    startup.cb = sizeof(startup);
    if (!CreateProcessA(NULL, command, NULL, NULL, FALSE, 0, NULL, NULL, &startup, &process)) return 2;
    DWORD wait = WaitForSingleObject(process.hProcess, 10000);
    CloseHandle(process.hThread);
    CloseHandle(process.hProcess);
    return wait == WAIT_OBJECT_0 ? 0 : 4;
}
