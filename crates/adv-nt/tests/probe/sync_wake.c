/* Build: i686-w64-mingw32-gcc -O1 -nostdlib -Wl,--entry,_mainCRTStartup -o sync-wake.exe sync_wake.c -lkernel32 */
#include <windows.h>

typedef LONG (WINAPI *alert_fn)(HANDLE);
typedef LONG (WINAPI *wait_alert_fn)(const void *, const LARGE_INTEGER *);
typedef LONG (WINAPI *test_alert_fn)(void);
typedef LONG (WINAPI *create_timer_fn)(HANDLE *, DWORD, void *, DWORD);
static alert_fn alert_thread;
static wait_alert_fn wait_alert;
static test_alert_fn test_alert;
static HANDLE timer, ready;
static DWORD main_id;
static LONG apcs;
static void CALLBACK apc(void *arg, DWORD low, DWORD high) { if (arg == (void *)42) ++apcs; }
static DWORD WINAPI alert_worker(void *arg)
{
    Sleep(1);
    return alert_thread((HANDLE)(ULONG_PTR)main_id) || alert_thread((HANDLE)(ULONG_PTR)main_id);
}
static DWORD WINAPI timer_worker(void *arg)
{
    SetEvent(ready);
    return WaitForSingleObject(timer, INFINITE);
}
static int run(void)
{
    HMODULE ntdll = GetModuleHandleA("ntdll.dll");
    HANDLE worker;
    LARGE_INTEGER zero = { .QuadPart = 0 }, due = { .QuadPart = -10000 };
    DWORD code;
    alert_thread = (alert_fn)GetProcAddress(ntdll, "NtAlertThreadByThreadId");
    wait_alert = (wait_alert_fn)GetProcAddress(ntdll, "NtWaitForAlertByThreadId");
    test_alert = (test_alert_fn)GetProcAddress(ntdll, "NtTestAlert");
    main_id = GetCurrentThreadId();
    if (alert_thread((HANDLE)(ULONG_PTR)main_id) || alert_thread((HANDLE)(ULONG_PTR)main_id)) return 1;
    if (wait_alert(NULL, &zero) != 0x101 || wait_alert(NULL, &zero) != 0x102) return 2;
    worker = CreateThread(NULL, 0, alert_worker, NULL, 0, NULL);
    if (wait_alert(NULL, NULL) != 0x101 || WaitForSingleObject(worker, INFINITE)) return 3;
    GetExitCodeThread(worker, &code);
    if (code || wait_alert(NULL, &zero) != 0x102) return 4;
    CloseHandle(worker);
    timer = CreateWaitableTimerA(NULL, FALSE, NULL);
    ready = CreateEventA(NULL, FALSE, FALSE, NULL);
    worker = CreateThread(NULL, 0, timer_worker, NULL, 0, NULL);
    if (WaitForSingleObject(ready, INFINITE)) return 5;
    Sleep(1);
    if (!SetWaitableTimer(timer, &due, 0, NULL, NULL, FALSE)) return 6;
    if (WaitForSingleObject(worker, INFINITE)) return 7;
    GetExitCodeThread(worker, &code);
    if (code) return 8;
    CloseHandle(worker);
    if (!SetWaitableTimer(timer, &due, 0, apc, (void *)42, FALSE)) return 9;
    if (SleepEx(INFINITE, TRUE) != WAIT_IO_COMPLETION || apcs != 1) return 10;
    if (!SetWaitableTimer(timer, &zero, 0, apc, (void *)42, FALSE)) return 11;
    if (test_alert() || apcs != 2) return 12;
    CloseHandle(timer);
    create_timer_fn create_timer = (create_timer_fn)GetProcAddress(ntdll, "NtCreateTimer");
    if (create_timer(&timer, 2, NULL, 1)) return 13;
    if (!SetWaitableTimer(timer, &due, 0, apc, (void *)42, FALSE)) return 14;
    if (SleepEx(INFINITE, TRUE) != WAIT_IO_COMPLETION || apcs != 3) return 15;
    CloseHandle(timer);
    CloseHandle(ready);
    static const char message[] = "alerts=1 timer=1 apcs=3\n";
    DWORD written;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), message, sizeof(message) - 1, &written, NULL);
    return 0;
}

void mainCRTStartup(void) { ExitProcess(run()); }
