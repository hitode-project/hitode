/* Authored loader/exception observer. Outputs only scalar contracts. */
#include <windows.h>
static DWORD values[512], count, mode;
static void flush(void) {
    DWORD written;
    HANDLE file = CreateFileA("exception-init.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file != INVALID_HANDLE_VALUE) {
        WriteFile(file, values, count * sizeof(DWORD), &written, NULL);
        CloseHandle(file);
    }
}
__declspec(dllexport) DWORD __cdecl Case(void) { return mode; }
__declspec(dllexport) void __cdecl Record(DWORD value) {
    if (count < 512) values[count++] = value;
    flush();
}
void mainCRTStartup(void) {
    char name[512]; DWORD i, error; HMODULE module;
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    GetModuleFileNameA(NULL, name, sizeof(name));
    for (i = 0; name[i]; ++i) {
        if (name[i]=='-' && name[i+1]>='0' && name[i+1]<='9') mode=name[i+1]-'0';
    }
    Record(0x1000 + mode);
    SetLastError(0);
    module = LoadLibraryA("exception-init.dll");
    error = GetLastError();
    Record(0x2000); Record(module != NULL); Record(module ? 0 : error);
    if (module) Record(FreeLibrary(module) != 0);
    Record(0x3000);
    ExitProcess(0);
}
