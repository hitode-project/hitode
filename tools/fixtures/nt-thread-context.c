/* Owned native/guest suspended-child context differential; no game data. */
#include <windows.h>
#ifdef CHILD
void mainCRTStartup(void) { ExitProcess(37); }
#else
typedef LONG (WINAPI *NT_GET_CONTEXT)(HANDLE, PCONTEXT);
static DWORD values[24];
static CONTEXT original, native, changed, checked;
static DWORD same_integer(const CONTEXT *a, const CONTEXT *b) {
    return a->Eax==b->Eax && a->Ebx==b->Ebx && a->Ecx==b->Ecx &&
        a->Edx==b->Edx && a->Esi==b->Esi && a->Edi==b->Edi;
}
void mainCRTStartup(void) {
    NT_GET_CONTEXT get = (NT_GET_CONTEXT)GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtGetContextThread");
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION info;
    HANDLE limited, file;
    DWORD exitcode=0, written;
    if (!get || !CreateProcessW(L"process-child.exe", NULL, NULL, NULL, FALSE,
        CREATE_SUSPENDED|CREATE_NO_WINDOW, NULL, NULL, &startup, &info)) ExitProcess(1);
    native.ContextFlags=CONTEXT_INTEGER; native.Dr0=0x12345678;
    SetLastError(0x13572468);
    values[0]=(DWORD)get((HANDLE)0x12345678, &native); values[1]=GetLastError();
    values[2]=(DWORD)get(info.hThread, &native);
    original.ContextFlags=CONTEXT_INTEGER; values[3]=GetThreadContext(info.hThread, &original);
    values[4]=same_integer(&native, &original); values[5]=native.Dr0==0x12345678;
    values[6]=native.ContextFlags==CONTEXT_INTEGER;
    changed.ContextFlags=CONTEXT_INTEGER;
    changed.Eax=original.Eax; changed.Ecx=original.Ecx; changed.Edx=original.Edx; changed.Edi=original.Edi;
    changed.Ebx=0x24681357; changed.Esi=0x76543210;
    values[7]=SetThreadContext(info.hThread, &changed); values[8]=(DWORD)get(info.hThread, &native);
    values[9]=native.Ebx==changed.Ebx && native.Esi==changed.Esi;
    checked.ContextFlags=CONTEXT_INTEGER; values[10]=GetThreadContext(info.hThread, &checked);
    values[11]=same_integer(&native, &checked);
    SetLastError(0x13572468); values[12]=(DWORD)get(info.hThread, NULL); values[13]=GetLastError();
    limited=NULL; values[14]=DuplicateHandle(GetCurrentProcess(), info.hThread,
        GetCurrentProcess(), &limited, THREAD_QUERY_INFORMATION, FALSE, 0);
    SetLastError(0x13572468); values[15]=(DWORD)get(limited, &native); values[16]=GetLastError();
    values[17]=CloseHandle(limited); values[18]=(DWORD)get(limited, &native);
    values[19]=SetThreadContext(info.hThread, &original); values[20]=ResumeThread(info.hThread);
    values[21]=WaitForSingleObject(info.hProcess, 5000);
    values[22]=GetExitCodeProcess(info.hProcess, &exitcode); values[23]=exitcode;
    CloseHandle(info.hThread); CloseHandle(info.hProcess);
    file=CreateFileW(L"nt-thread-context.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file==INVALID_HANDLE_VALUE || !WriteFile(file, values, sizeof(values), &written, NULL) || written!=sizeof(values)) ExitProcess(2);
    CloseHandle(file); ExitProcess(0);
}
#endif
