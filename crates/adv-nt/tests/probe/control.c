/* The thread-control probe: a busy-wait on the tick count, which makes no
 * system call, a spinning thread that is suspended, resumed and terminated,
 * and a waiting thread that is terminated. It exits with 0 only when every
 * step did what Windows does.
 * Build: i686-w64-mingw32-gcc -O1 -o control.exe control.c */
#include <windows.h>
#include <stdio.h>

static volatile LONG spins;
static HANDLE never;

static DWORD WINAPI spinner(void *parameter)
{
    for (;;) InterlockedIncrement(&spins);
}

static DWORD WINAPI waiter(void *parameter)
{
    WaitForSingleObject(never, INFINITE);
    return 1;
}

int main(void)
{
    HANDLE spinning, waiting;
    DWORD start, suspended, resumed, spun, frozen, ran, joined, code, waited, waiter_code;
    LONG before, after;

    start = GetTickCount();
    while (GetTickCount() - start < 20)
        ;

    spinning = CreateThread(NULL, 0, spinner, NULL, 0, NULL);
    while (!spins) Sleep(1);
    suspended = SuspendThread(spinning);
    Sleep(5);
    before = spins;
    Sleep(20);
    after = spins;
    frozen = before == after;
    resumed = ResumeThread(spinning);
    while (spins == after) Sleep(1);
    ran = spins != after;
    TerminateThread(spinning, 3);
    joined = WaitForSingleObject(spinning, INFINITE);
    GetExitCodeThread(spinning, &code);
    CloseHandle(spinning);

    never = CreateEventA(NULL, TRUE, FALSE, NULL);
    waiting = CreateThread(NULL, 0, waiter, NULL, 0, NULL);
    Sleep(5);
    TerminateThread(waiting, 4);
    waited = WaitForSingleObject(waiting, INFINITE);
    GetExitCodeThread(waiting, &waiter_code);
    CloseHandle(waiting);

    spun = GetTickCount() - start >= 20;
    printf("ticked=%lu suspended=%lu frozen=%lu resumed=%lu ran=%lu joined=%lu code=%lu "
           "waited=%lu waiter=%lu\n",
           spun, suspended, frozen, resumed, ran, joined, code, waited, waiter_code);
    return spun && suspended == 0 && frozen && resumed == 1 && ran && joined == WAIT_OBJECT_0
                   && code == 3 && waited == WAIT_OBJECT_0 && waiter_code == 4
               ? 0
               : 1;
}
