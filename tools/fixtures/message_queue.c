/* Native-compatible thread-message new/old state and nine-object wait checks.
 * No visible window, audio device, or game resources. Exit code identifies a failed check. */
#include "win32_min.h"
API DWORD WINAPI GetCurrentThreadId(void);
API BOOL WINAPI PeekMessageA(MSG *, HWND, UINT, UINT, UINT);
API BOOL WINAPI PostThreadMessageA(DWORD, UINT, WPARAM, LPARAM);
API BOOL WINAPI PostMessageA(HWND, UINT, WPARAM, LPARAM);
API DWORD WINAPI MsgWaitForMultipleObjects(DWORD, const HANDLE *, BOOL, DWORD, DWORD);
API DWORD WINAPI GetQueueStatus(UINT);
API DWORD WINAPI WaitForMultipleObjects(DWORD, const HANDLE *, BOOL, DWORD);
API BOOL WINAPI ResetEvent(HANDLE);
API BOOL WINAPI GetExitCodeThread(HANDLE, DWORD *);
typedef struct { MSG message; DWORD private_value; } NATIVE_MSG;
static DWORD test_number;
static DWORD main_thread;
static HANDLE grant_events[9], grant_arrived;
static DWORD grant_mode;
static DWORD WINAPI grant_waiter(void *unused) {
    NATIVE_MSG out = {0};
    (void)unused;
    PeekMessageA(&out.message, 0, 0, 0, 1);
    SetEvent(grant_arrived);
    if (grant_mode == 0)
        return WaitForMultipleObjects(9, grant_events, 0, 1000);
    return MsgWaitForMultipleObjects(9, grant_events, grant_mode == 2, 1000, 8);
}
static DWORD WINAPI delayed_post(void *unused) {
    (void)unused;
    Sleep(10);
    PostThreadMessageA(main_thread, 0x513, 22, 0);
    return 0;
}
static void check(DWORD actual, DWORD expected) {
    ++test_number;
    if (actual != expected) ExitProcess(test_number);
}
static DWORD wait(const HANDLE *events, DWORD count, BOOL all, DWORD mask) {
    return MsgWaitForMultipleObjects(count, events, all, 0, mask);
}
static void grant_checks(void) {
    DWORD mode, i, tid, code;
    HANDLE thread;
    for (mode = 0; mode < 3; ++mode) {
        grant_mode = mode;
        grant_arrived = CreateEventA(NULL, 0, 0, NULL);
        for (i = 0; i < 9; ++i) grant_events[i] = CreateEventA(NULL, 0, 0, NULL);
        thread = CreateThread(NULL, 0, grant_waiter, NULL, 0, &tid);
        check(WaitForSingleObject(grant_arrived, 1000), 0);
        Sleep(20); /* Let the waiter enter its wait, also on native Windows. */
        if (mode != 2) {
            SetEvent(grant_events[7]);
            check(WaitForSingleObject(grant_events[7], 0), 0x102);
            ResetEvent(grant_events[7]); /* Must not revoke a completed grant. */
            SetEvent(grant_events[2]);
            check(WaitForSingleObject(thread, 1000), 0);
            check(GetExitCodeThread(thread, &code), 1);
            check(code, 7);
            check(WaitForSingleObject(grant_events[2], 0), 0);
        } else {
            for (i = 0; i < 9; ++i) SetEvent(grant_events[i]);
            check(WaitForSingleObject(thread, 0), 0x102); /* Input is still absent. */
            check(WaitForSingleObject(grant_events[3], 0), 0); /* No partial reservation. */
            check(PostThreadMessageA(tid, 0x506, 31, 0), 1);
            Sleep(20); /* USER32 re-enters its object wait after the input wake. */
            check(WaitForSingleObject(thread, 0), 0x102); /* One object is now missing. */
            SetEvent(grant_events[3]);
            check(WaitForSingleObject(thread, 1000), 0);
            check(GetExitCodeThread(thread, &code), 1);
            check(code, 0);
            for (i = 0; i < 9; ++i) check(WaitForSingleObject(grant_events[i], 0), 0x102);
        }
        CloseHandle(thread); CloseHandle(grant_arrived);
        for (i = 0; i < 9; ++i) CloseHandle(grant_events[i]);
    }
}
void mainCRTStartup(void) {
    NATIVE_MSG out = {0};
    DWORD self = GetCurrentThreadId(), i;
    HANDLE events[9];
    PeekMessageA(&out.message, 0, 0, 0, 1); /* Create native queue. */
    check(PostThreadMessageA(self, 0x506, 10, 0), 1);
    check(wait(NULL, 0, 0, 8), 0);
    check(wait(NULL, 0, 0, 8), 0); /* Waiting alone does not mark old. */
    check(PeekMessageA(&out.message, 0, 0, 0, 0), 1);
    check(wait(NULL, 0, 0, 8), 0x102);
    check(wait(NULL, 0, 0, 0x100), 0x102);
    check(PostThreadMessageA(self, 0x513, 11, 0), 1);
    check(wait(NULL, 0, 0, 8), 0);
    check(PeekMessageA(&out.message, 0, 0x599, 0x599, 0), 0);
    check(wait(NULL, 0, 0, 8), 0x102);
    check(wait(NULL, 0, 0, 0x100), 0);
    check(GetQueueStatus(0x108), 0x01080100);
    check(GetQueueStatus(0x108), 0x01080000);
    check(wait(NULL, 0, 0, 0x100), 0x102);
    while (PeekMessageA(&out.message, 0, 0, 0, 1)) {}

    PostThreadMessageA(self, 0x501, 1, 0);
    PostThreadMessageA(self, 0x506, 2, 0);
    PostThreadMessageA(self, 0x513, 3, 0);
    PostThreadMessageA(self, 0x506, 4, 0);
    PostThreadMessageA(self, 0x513, 5, 0);
    check(PeekMessageA(&out.message, 0, 0x506, 0x506, 1), 1);
    check(out.message.message, 0x506); check(out.message.wparam, 2);
    check(GetMessageA(&out.message, 0, 0, 0), 1);
    check(out.message.message, 0x501); check(out.message.wparam, 1);
    check(GetMessageA(&out.message, 0, 0x513, 0x513), 1);
    check(out.message.message, 0x513); check(out.message.wparam, 3);
    check(PeekMessageA(&out.message, 0, 0, 0, 1), 1);
    check(out.message.message, 0x506); check(out.message.wparam, 4);
    check(PeekMessageA(&out.message, 0, 0, 0, 1), 1);
    check(out.message.message, 0x513); check(out.message.wparam, 5);
    check(PeekMessageA(&out.message, 0, 0, 0, 1), 0);

    {
        UINT messages[] = { 0x100, 0x102, 0x200, 0xf, 0x113, 0x506 };
        for (i = 0; i < 6; ++i) {
            PostThreadMessageA(self, messages[i], 0, 0);
            check(GetQueueStatus(0x13f), 0x01080108);
            check(wait(NULL, 0, 0, 1 | 2 | 4 | 0x10 | 0x20), 0x102);
            PeekMessageA(&out.message, 0, 0, 0, 1);
            PostMessageA(0, messages[i], 0, 0);
            check(GetQueueStatus(0x13f), 0x01080108);
            PeekMessageA(&out.message, 0, 0, 0, 1);
        }
    }
    {
        WNDCLASSA wc = {0}; HWND hwnd;
        wc.procedure = DefWindowProcA; wc.name = "QueueTranslateCheck";
        wc.instance = GetModuleHandleA(NULL); RegisterClassA(&wc);
        hwnd = CreateWindowExA(0, wc.name, "", 0, 0, 0, 32, 32, 0, 0, wc.instance, NULL);
        check(hwnd != 0, 1);
        while (PeekMessageA(&out.message, 0, 0, 0, 1)) {}
        PostMessageA(hwnd, 0x100, 0x41, 0x001e0001);
        check(PeekMessageA(&out.message, 0, 0, 0, 1), 1);
        check(TranslateMessage(&out.message), 1);
        check(GetQueueStatus(0x13f), 0x01080108);
        check(PeekMessageA(&out.message, 0, 0, 0, 1), 1);
        check(out.message.message, 0x102);
        DestroyWindow(hwnd);
    }

    for (i = 0; i < 9; ++i) events[i] = CreateEventA(NULL, 0, 0, NULL);
    PostThreadMessageA(self, 0x506, 20, 0);
    SetEvent(events[7]); SetEvent(events[2]);
    check(wait(events, 9, 0, 8), 9); /* New input takes priority. */
    check(wait(events, 9, 0, 8), 9);
    check(PeekMessageA(&out.message, 0, 0, 0, 0), 1);
    check(wait(events, 9, 0, 8), 2); /* Old input no longer takes priority. */
    check(wait(events, 9, 0, 8), 7);
    check(wait(events, 9, 0, 8), 0x102);
    for (i = 0; i < 9; ++i) SetEvent(events[i]);
    check(wait(events, 9, 1, 8), 0x102); /* WaitAll also requires new input. */
    PostThreadMessageA(self, 0x513, 21, 0);
    check(wait(events, 9, 1, 8), 0);
    check(wait(events, 9, 0, 8), 9); /* WaitAll consumed objects, not newness. */
    PeekMessageA(&out.message, 0, 0, 0, 0);
    check(wait(events, 9, 0, 8), 0x102);
    for (i = 0; i < 9; ++i) CloseHandle(events[i]);
    while (PeekMessageA(&out.message, 0, 0, 0, 1)) {}
    PostThreadMessageA(self, 0x506, 21, 0);
    PeekMessageA(&out.message, 0, 0, 0, 0); /* Leave old input pending. */
    {
        HANDLE thread;
        main_thread = self;
        thread = CreateThread(NULL, 0, delayed_post, NULL, 0, NULL);
        check(thread != 0, 1);
        check(MsgWaitForMultipleObjects(0, NULL, 0, 1000, 8), 0);
        check(GetQueueStatus(0x108), 0x01080108);
        check(PeekMessageA(&out.message, 0, 0, 0, 1), 1);
        check(out.message.wparam, 21); /* Newness does not change FIFO. */
        check(PeekMessageA(&out.message, 0, 0, 0, 1), 1);
        check(out.message.wparam, 22);
        check(WaitForSingleObject(thread, 1000), 0);
        CloseHandle(thread);
    }
    grant_checks();
    ExitProcess(0);
}
