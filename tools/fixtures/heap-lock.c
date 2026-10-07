/* Authored x86 heap-lock oracle; no game data.
 * Build/run with test-heap-lock.ps1. No input; writes heap-lock.bin with
 * 105 stable scalar results and two advisory EAX upper-word observations.
 * BOOLEAN callers consume AL; tests do not assume unspecified upper bits.
 */
#include <windows.h>
typedef BYTE(WINAPI *RTL_LOCK)(HANDLE);
typedef PVOID(WINAPI *RTL_ALLOCATE)(HANDLE, ULONG, SIZE_T);
typedef ULONG(WINAPI *RTL_FREE)(HANDLE, ULONG, PVOID);
static RTL_LOCK rtl_lock, rtl_unlock;
static RTL_ALLOCATE rtl_allocate;
static RTL_FREE rtl_free;
static HANDLE heap, start[8], attempted[8], finished[8];
static PVOID blocks[8];
static DWORD values[107];
#define SENTINEL 0x13572468

/* Observe the real EAX after a correctly framed stdcall1 call. */
__declspec(naked) static DWORD WINAPI raw_call(RTL_LOCK function, HANDLE handle) {
    __asm {
        mov edx, dword ptr [esp + 4]
        mov ecx, dword ptr [esp + 8]
        push ecx
        mov eax, 0xaabbcc00
        call edx
        ret 8
    }
}

static DWORD WINAPI worker(LPVOID parameter) {
    DWORD mode = (DWORD)parameter, base = 16 + 11 * mode;
    DWORD flags = mode & 2 ? HEAP_NO_SERIALIZE : 0;
    WaitForSingleObject(start[mode], INFINITE);
    SetEvent(attempted[mode]);
    SetLastError(SENTINEL);
    if (mode & 1) {
        values[base + 7] = mode & 4 ? rtl_free(heap, flags, blocks[mode]) : HeapFree(heap, flags, blocks[mode]);
        values[base + 8] = GetLastError();
        values[base + 9] = 1;
    } else {
        PVOID pointer = mode & 4 ? rtl_allocate(heap, flags, 37) : HeapAlloc(heap, flags, 37);
        values[base + 7] = pointer != NULL;
        values[base + 8] = GetLastError();
        values[base + 9] = pointer ? (mode & 4 ? rtl_free(heap, flags, pointer) : HeapFree(heap, flags, pointer)) : 0;
    }
    values[base + 10] = GetLastError();
    SetEvent(finished[mode]);
    return 0;
}

void mainCRTStartup(void) {
    HMODULE nt = GetModuleHandleW(L"ntdll.dll");
    HANDLE threads[8], file;
    PVOID pointer;
    DWORD raw, mode, written;
    rtl_lock = (RTL_LOCK)GetProcAddress(nt, "RtlLockHeap");
    rtl_unlock = (RTL_LOCK)GetProcAddress(nt, "RtlUnlockHeap");
    rtl_allocate = (RTL_ALLOCATE)GetProcAddress(nt, "RtlAllocateHeap");
    rtl_free = (RTL_FREE)GetProcAddress(nt, "RtlFreeHeap");
    values[0] = rtl_lock != NULL && rtl_unlock != NULL && rtl_allocate != NULL && rtl_free != NULL;
    if (!values[0]) ExitProcess(1);
    heap = HeapCreate(0, 0, 0);
    values[1] = heap != NULL;
    if (!heap) ExitProcess(2);
    SetLastError(SENTINEL);
    raw = raw_call(rtl_lock, heap);
    values[2] = (raw & 255) == 1;
    values[105] = raw >> 8;
    values[3] = GetLastError();
    values[4] = rtl_lock(heap);
    values[5] = GetLastError();
    values[6] = HeapLock(heap);
    values[7] = GetLastError();
    pointer = HeapAlloc(heap, 0, 29);
    values[8] = pointer != NULL;
    values[9] = pointer ? HeapFree(heap, 0, pointer) : 0;
    values[10] = rtl_unlock(heap);
    values[11] = HeapUnlock(heap);
    raw = raw_call(rtl_unlock, heap);
    values[12] = (raw & 255) == 1;
    values[106] = raw >> 8;
    values[13] = GetLastError();
    values[14] = HeapLock(heap);
    values[15] = rtl_unlock(heap);

    for (mode = 0; mode != 8; ++mode) {
        if (mode & 1) blocks[mode] = HeapAlloc(heap, 0, 43);
        if ((mode & 1) && !blocks[mode]) ExitProcess(3);
        start[mode] = CreateEventW(NULL, FALSE, FALSE, NULL);
        attempted[mode] = CreateEventW(NULL, FALSE, FALSE, NULL);
        finished[mode] = CreateEventW(NULL, TRUE, FALSE, NULL);
        threads[mode] = CreateThread(NULL, 0, worker, (LPVOID)mode, 0, NULL);
        if (!start[mode] || !attempted[mode] || !finished[mode] || !threads[mode]) ExitProcess(4);
    }
    for (mode = 0; mode != 8; ++mode) {
        DWORD base = 16 + 11 * mode;
        if (!rtl_lock(heap) || !HeapLock(heap)) ExitProcess(5);
        SetEvent(start[mode]);
        values[base] = WaitForSingleObject(attempted[mode], 5000);
        values[base + 1] = WaitForSingleObject(finished[mode], 25);
        values[base + 2] = rtl_unlock(heap);
        values[base + 3] = WaitForSingleObject(finished[mode], 25);
        values[base + 4] = HeapUnlock(heap);
        values[base + 5] = WaitForSingleObject(finished[mode], 5000);
        values[base + 6] = WaitForSingleObject(threads[mode], 5000);
        CloseHandle(threads[mode]);
        CloseHandle(start[mode]);
        CloseHandle(attempted[mode]);
        CloseHandle(finished[mode]);
    }
    values[104] = HeapDestroy(heap);
    file = CreateFileW(L"heap-lock.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, sizeof(values), &written, NULL) || written != sizeof(values)) ExitProcess(6);
    CloseHandle(file);
    ExitProcess(0);
}
