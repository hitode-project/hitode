/* Build: i686-w64-mingw32-gcc -O1 -o inproc.exe inproc.c */
#include <windows.h>
#include <stdio.h>

static HANDLE sem, event;
static DWORD WINAPI worker(void *unused)
{
    unsigned int i;
    (void)unused;
    if (!SetEvent(event)) return 3;
    for (i = 0; i < 10000; i++)
    {
        if (WaitForSingleObject(sem, INFINITE) != WAIT_OBJECT_0) return 1;
        if (!SetEvent(event)) return 2;
    }
    return 0;
}
int main(void)
{
    unsigned int i;
    DWORD code;
    HANDLE thread;
    sem = CreateSemaphoreW(NULL, 0, 1, NULL);
    event = CreateEventW(NULL, FALSE, FALSE, NULL);
    thread = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    if (!sem || !event || !thread) return 1;
    if (WaitForSingleObject(event, INFINITE) != WAIT_OBJECT_0) return 6;
    for (i = 0; i < 10000; i++)
    {
        if (!ReleaseSemaphore(sem, 1, NULL)) return 2;
        if (WaitForSingleObject(event, INFINITE) != WAIT_OBJECT_0) return 3;
    }
    if (WaitForSingleObject(thread, INFINITE) != WAIT_OBJECT_0) return 4;
    if (!GetExitCodeThread(thread, &code) || code) return 5;
    CloseHandle(thread); CloseHandle(sem); CloseHandle(event);
    {
        /* File I/O signals its event as NtSetEvent would. */
        WCHAR path[MAX_PATH];
        char bytes[2];
        OVERLAPPED overlapped = {0};
        HANDLE file;
        GetModuleFileNameW(NULL, path, MAX_PATH);
        file = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
        overlapped.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
        if (!ReadFile(file, bytes, 2, NULL, &overlapped)) return 7;
        if (WaitForSingleObject(overlapped.hEvent, 0) != WAIT_OBJECT_0) return 8;
        CloseHandle(overlapped.hEvent); CloseHandle(file);
        printf("rounds=%u code=%lu read=%.2s\n", i, code, bytes);
    }
    return 0;
}
