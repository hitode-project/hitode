#include <windows.h>
#include <winternl.h>

typedef NTSTATUS (NTAPI *LOCK)(ULONG, PULONG, PVOID *);
typedef NTSTATUS (NTAPI *UNLOCK)(ULONG, PVOID);
static LOCK lock_loader;
static UNLOCK unlock_loader;
static HANDLE ready, go, done, retry;
static DWORD values[24];
static DWORD WINAPI worker(void *unused) {
    ULONG disposition = 99;
    PVOID cookie = (PVOID)99;
    SetEvent(ready);
    WaitForSingleObject(go, INFINITE);
    SetLastError(0x12345678);
    values[8] = (DWORD)lock_loader(2, &disposition, &cookie);
    values[9] = disposition;
    values[10] = cookie == NULL;
    values[11] = GetLastError();
    SetEvent(done);
    WaitForSingleObject(retry, INFINITE);
    values[21] = (DWORD)lock_loader(0, &disposition, &cookie);
    values[22] = disposition;
    if (!cookie) ExitProcess(1);
    values[23] = (DWORD)unlock_loader(0, cookie);
    return 0;
}
void mainCRTStartup(void) {
    HMODULE nt = GetModuleHandleW(L"ntdll.dll");
    ULONG disposition = 99;
    PVOID first = NULL, second = NULL;
    HANDLE thread, file;
    DWORD written;
    lock_loader = (LOCK)GetProcAddress(nt, "LdrLockLoaderLock");
    unlock_loader = (UNLOCK)GetProcAddress(nt, "LdrUnlockLoaderLock");
    if (!lock_loader || !unlock_loader) ExitProcess(2);
    ready = CreateEventW(NULL, FALSE, FALSE, NULL);
    go = CreateEventW(NULL, FALSE, FALSE, NULL);
    done = CreateEventW(NULL, FALSE, FALSE, NULL);
    retry = CreateEventW(NULL, FALSE, FALSE, NULL);
    thread = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    if (!thread || WaitForSingleObject(ready, 5000) != WAIT_OBJECT_0) ExitProcess(3);
    SetLastError(0x12345678);
    values[0] = (DWORD)lock_loader(0, &disposition, &first);
    values[1] = disposition;
    values[2] = first != NULL;
    values[3] = GetLastError();
    nt = LoadLibraryW(L"kernel32.dll");
    if (!nt || !FreeLibrary(nt)) ExitProcess(7);
    values[4] = (DWORD)lock_loader(2, &disposition, &second);
    values[5] = disposition;
    values[6] = second != NULL;
    values[7] = (DWORD)unlock_loader(0, second);
    SetEvent(go);
    if (WaitForSingleObject(done, 5000) != WAIT_OBJECT_0) ExitProcess(4);
    values[12] = (DWORD)unlock_loader(0, first);
    values[13] = (DWORD)unlock_loader(0, NULL);
    disposition = 99; second = (PVOID)99;
    values[14] = (DWORD)lock_loader(4, &disposition, &second);
    values[15] = disposition;
    values[16] = second == NULL;
    disposition = 99;
    values[17] = (DWORD)lock_loader(0, &disposition, NULL);
    values[18] = disposition;
    second = (PVOID)99;
    values[19] = (DWORD)lock_loader(2, NULL, &second);
    values[20] = second == NULL;
    SetEvent(retry);
    if (WaitForSingleObject(thread, 5000) != WAIT_OBJECT_0) ExitProcess(5);
    CloseHandle(thread);
    CloseHandle(ready); CloseHandle(go); CloseHandle(done); CloseHandle(retry);
    file = CreateFileW(L"ldr-loader-lock.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, sizeof(values), &written, NULL) || written != sizeof(values)) ExitProcess(6);
    CloseHandle(file);
    ExitProcess(0);
}
