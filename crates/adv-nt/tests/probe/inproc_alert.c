/* Build: i686-w64-mingw32-gcc -O1 -o inproc_alert.exe inproc_alert.c */
#include <windows.h>
#include <stdio.h>
static HANDLE target, sem;
static ULONG_PTR argument;
static void CALLBACK apc(ULONG_PTR value) { argument = value; }
static DWORD WINAPI worker(void *unused)
{
    (void)unused;
    if (WaitForSingleObject(sem, INFINITE) != WAIT_OBJECT_0) return 1;
    return QueueUserAPC(apc, target, 42) ? 0 : 2;
}
int main(void)
{
    HANDLE thread, event;
    DWORD code;
    target = OpenThread(THREAD_SET_CONTEXT, FALSE, GetCurrentThreadId());
    sem = CreateSemaphoreW(NULL, 0, 1, NULL);
    event = CreateEventW(NULL, FALSE, FALSE, NULL);
    thread = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    if (!target || !sem || !event || !thread) return 1;
    if (!ReleaseSemaphore(sem, 1, NULL)) return 2;
    if (WaitForSingleObjectEx(event, INFINITE, TRUE) != WAIT_IO_COMPLETION) return 3;
    if (argument != 42 || WaitForSingleObject(event, 0) != WAIT_TIMEOUT) return 4;
    if (WaitForSingleObject(thread, INFINITE) != WAIT_OBJECT_0) return 5;
    if (!GetExitCodeThread(thread, &code) || code) return 6;
    printf("apc=%lu code=%lu\n", (unsigned long)argument, code);
    CloseHandle(thread); CloseHandle(event); CloseHandle(sem); CloseHandle(target);
    return 0;
}
