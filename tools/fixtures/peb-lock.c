/* Authored x86 PEB-lock oracle; no game data.
 * Build and run with test-peb-lock.ps1. No input; writes 51 scalar results to
 * peb-lock.bin. Checks recursion, Win32 interop, two-way contention and
 * LastError without exposing pointers or comparing native LockCount encoding.
 */
#include <windows.h>
#include <intrin.h>

typedef VOID(WINAPI *PEB_LOCK)(VOID);
static PEB_LOCK acquire_peb, release_peb;
static CRITICAL_SECTION *section;
static HANDLE start[2], attempted[2], acquired[2];
static volatile LONG worker_entered[2];
static DWORD values[51];
#define SENTINEL 0x13572468

static DWORD WINAPI worker(LPVOID parameter) {
    DWORD mode = (DWORD)parameter;
    DWORD base = 24 + 13 * mode;
    WaitForSingleObject(start[mode], INFINITE);
    SetEvent(attempted[mode]);
    SetLastError(SENTINEL);
    if (mode) EnterCriticalSection(section); else acquire_peb();
    values[base + 9] = GetLastError();
    values[base + 7] = (DWORD)section->OwningThread == GetCurrentThreadId();
    values[base + 8] = section->RecursionCount == 1;
    InterlockedExchange(&worker_entered[mode], 1);
    SetEvent(acquired[mode]);
    SetLastError(SENTINEL);
    if (mode) release_peb(); else LeaveCriticalSection(section);
    values[base + 10] = GetLastError();
    return 0;
}

void mainCRTStartup(void) {
    HMODULE nt = GetModuleHandleW(L"ntdll.dll");
    HANDLE threads[2], file;
    DWORD mode, written;
    acquire_peb = (PEB_LOCK)GetProcAddress(nt, "RtlAcquirePebLock");
    release_peb = (PEB_LOCK)GetProcAddress(nt, "RtlReleasePebLock");
    values[0] = acquire_peb != NULL && release_peb != NULL;
    if (!values[0]) ExitProcess(1);
    section = *(CRITICAL_SECTION **)(__readfsdword(0x30) + 0x1c);
    values[1] = section != NULL;
    if (!values[1]) ExitProcess(2);
    values[2] = section->RecursionCount == 0;
    values[3] = section->OwningThread == NULL;

    SetLastError(SENTINEL);
    acquire_peb();
    values[4] = section->RecursionCount == 1;
    values[5] = (DWORD)section->OwningThread == GetCurrentThreadId();
    values[6] = GetLastError();
    EnterCriticalSection(section);
    values[7] = section->RecursionCount == 2;
    values[8] = GetLastError();
    values[9] = TryEnterCriticalSection(section);
    values[10] = section->RecursionCount == 3;
    values[11] = GetLastError();
    LeaveCriticalSection(section);
    values[12] = section->RecursionCount == 2;
    values[13] = GetLastError();
    release_peb();
    values[14] = section->RecursionCount == 1;
    values[15] = GetLastError();
    LeaveCriticalSection(section);
    values[16] = section->RecursionCount == 0;
    values[17] = section->OwningThread == NULL;
    values[18] = GetLastError();

    EnterCriticalSection(section);
    values[19] = section->RecursionCount == 1;
    acquire_peb();
    values[20] = section->RecursionCount == 2;
    release_peb();
    values[21] = section->RecursionCount == 1;
    LeaveCriticalSection(section);
    values[22] = section->RecursionCount == 0;
    values[23] = GetLastError();

    for (mode = 0; mode != 2; ++mode) {
        start[mode] = CreateEventW(NULL, FALSE, FALSE, NULL);
        attempted[mode] = CreateEventW(NULL, FALSE, FALSE, NULL);
        acquired[mode] = CreateEventW(NULL, FALSE, FALSE, NULL);
        if (!start[mode] || !attempted[mode] || !acquired[mode]) ExitProcess(3);
        threads[mode] = CreateThread(NULL, 0, worker, (LPVOID)mode, 0, NULL);
        if (!threads[mode]) ExitProcess(4);
    }
    for (mode = 0; mode != 2; ++mode) {
        DWORD base = 24 + 13 * mode;
        if (mode) EnterCriticalSection(section); else acquire_peb();
        SetEvent(start[mode]);
        values[base] = WaitForSingleObject(attempted[mode], 5000);
        values[base + 1] = WaitForSingleObject(acquired[mode], 25);
        values[base + 2] = InterlockedCompareExchange(&worker_entered[mode], 0, 0);
        values[base + 3] = (DWORD)section->OwningThread == GetCurrentThreadId();
        SetLastError(SENTINEL);
        if (mode) release_peb(); else LeaveCriticalSection(section);
        values[base + 4] = GetLastError();
        values[base + 5] = WaitForSingleObject(acquired[mode], 5000);
        values[base + 6] = WaitForSingleObject(threads[mode], 5000);
        values[base + 11] = section->RecursionCount == 0;
        values[base + 12] = section->OwningThread == NULL;
        CloseHandle(threads[mode]);
        CloseHandle(start[mode]);
        CloseHandle(attempted[mode]);
        CloseHandle(acquired[mode]);
    }
    values[50] = 0x50454231;
    file = CreateFileW(L"peb-lock.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE ||
        !WriteFile(file, values, sizeof(values), &written, NULL) ||
        written != sizeof(values)) ExitProcess(5);
    CloseHandle(file);
    ExitProcess(0);
}
