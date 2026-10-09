/* Authored loader/exception observer. Outputs only scalar contracts. */
#include <windows.h>
static DWORD values[512], count, mode, attempt;
typedef struct Registration { struct Registration *previous; void *handler; } Registration;
typedef struct Unicode { USHORT length, maximum; WCHAR *buffer; } Unicode;
typedef LONG (WINAPI *LDR_LOAD)(WCHAR *, DWORD *, Unicode *, HMODULE *);
static void flush(void) {
    DWORD written;
    HANDLE file = CreateFileA("exception-init.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file != INVALID_HANDLE_VALUE) {
        WriteFile(file, values, count * sizeof(DWORD), &written, NULL);
        CloseHandle(file);
    }
}
__declspec(dllexport) DWORD __cdecl Case(void) { return mode; }
__declspec(dllexport) DWORD __cdecl Attempt(void) { return attempt; }
__declspec(dllexport) void __cdecl Record(DWORD value) {
    if (count < 512) values[count++] = value;
    flush();
}
static LONG CALLBACK vector(EXCEPTION_POINTERS *p) {
    Record(0x601); Record(p->ExceptionRecord->ExceptionCode);
    Record(p->ExceptionRecord->ExceptionFlags & 7);
    return EXCEPTION_CONTINUE_SEARCH;
}
static LONG __cdecl outer(EXCEPTION_RECORD *r, void *f, CONTEXT *c, void *d) {
    (void)r; (void)f; (void)c; (void)d;
    Record(0x701);
    return 1;
}
void mainCRTStartup(void) {
    char name[512]; DWORD i, error; HMODULE module;
    Registration registration; void *observer = NULL;
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    GetModuleFileNameA(NULL, name, sizeof(name));
    for (i = 0; name[i]; ++i) {
        if (name[i]=='-' && name[i+1]>='0' && name[i+1]<='9') {
            DWORD j = i + 1; mode = 0;
            while (name[j]>='0' && name[j]<='9') mode=mode*10+name[j++]-'0';
        }
    }
    Record(0x1000 + mode);
    if (mode >= 10) {
        observer = AddVectoredExceptionHandler(1, vector);
        registration.handler = outer;
        __asm__ volatile("movl %%fs:0,%0" : "=r"(registration.previous) : : "memory");
        __asm__ volatile("movl %0,%%fs:0" : : "r"(&registration) : "memory");
    }
    SetLastError(0);
    if (mode == 13) {
        WCHAR path[] = L"exception-init.dll";
        Unicode string = {sizeof(path)-2, sizeof(path), path};
        LDR_LOAD load = (LDR_LOAD)GetProcAddress(GetModuleHandleA("ntdll.dll"), "LdrLoadDll");
        module = NULL;
        Record((DWORD)load(NULL, NULL, &string, &module));
    } else module = LoadLibraryA("exception-init.dll");
    error = GetLastError();
    Record(0x2000); Record(module != NULL); Record(module || mode == 13 ? 0 : error);
    if (module) Record(FreeLibrary(module) != 0);
    if (mode >= 10) {
        Record(GetModuleHandleA("exception-init.dll") == NULL);
        attempt = 1;
        module = LoadLibraryA("exception-init.dll");
        Record(module != NULL);
        if (module) Record(FreeLibrary(module) != 0);
        Record(GetModuleHandleA("exception-init.dll") == NULL);
        __asm__ volatile("movl %0,%%fs:0" : : "r"(registration.previous) : "memory");
        Record(RemoveVectoredExceptionHandler(observer) != 0);
    }
    Record(0x3000);
    ExitProcess(0);
}
