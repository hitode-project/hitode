/* The gate-2 probe: two threads that wake each other, a wait that times
 * out, and an alertable sleep that a queued APC ends. It exits with 0 only
 * when every step did what Windows does.
 * Build: i686-w64-mingw32-gcc -O1 -o gate2.exe gate2.c */
#include <windows.h>
#include <stdio.h>

static HANDLE go, ready, done;
static volatile LONG apcs;
static ULONG_PTR apc_argument;

static void CALLBACK apc(ULONG_PTR argument)
{
    apc_argument = argument;
    InterlockedIncrement(&apcs);
}

static DWORD WINAPI worker(void *parameter)
{
    DWORD woken, timed_out, alerted;

    /* The main thread sets it after it sleeps. */
    woken = WaitForSingleObject(go, INFINITE);
    timed_out = WaitForSingleObject(done, 5);
    SetEvent(ready);
    alerted = SleepEx(INFINITE, TRUE);
    return woken == WAIT_OBJECT_0 && timed_out == WAIT_TIMEOUT && alerted == WAIT_IO_COMPLETION
               ? 7
               : 1;
}

int main(void)
{
    HANDLE thread, both[2];
    DWORD id, ready_wait, joined, code = 0;

    go = CreateEventA(NULL, FALSE, FALSE, NULL);
    ready = CreateEventA(NULL, TRUE, FALSE, NULL);
    done = CreateEventA(NULL, TRUE, FALSE, NULL);
    thread = CreateThread(NULL, 0, worker, NULL, 0, &id);
    Sleep(10);
    SetEvent(go);
    both[0] = ready;
    both[1] = done;
    ready_wait = WaitForMultipleObjects(2, both, FALSE, INFINITE);
    QueueUserAPC(apc, thread, 42);
    joined = WaitForSingleObject(thread, INFINITE);
    GetExitCodeThread(thread, &code);
    CloseHandle(thread);
    printf("ready=%lu joined=%lu code=%lu apcs=%ld argument=%lu\n", ready_wait, joined, code,
           apcs, (unsigned long)apc_argument);
    return ready_wait == WAIT_OBJECT_0 && joined == WAIT_OBJECT_0 && code == 7 && apcs == 1
                   && apc_argument == 42
               ? 0
               : 1;
}
