/* Native Windows USER32 queue semantics; no game or ADV guest code executes here. */
#include "win32_min.h"
#define TRUE 1
#define FALSE 0
API HANDLE WINAPI GetStdHandle(DWORD);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
API DWORD WINAPI GetCurrentThreadId(void);
API DWORD WINAPI GetLastError(void);
API void WINAPI SetLastError(DWORD);
API BOOL WINAPI PeekMessageA(MSG *, HWND, UINT, UINT, UINT);
API BOOL WINAPI PostThreadMessageA(DWORD, UINT, WPARAM, LPARAM);
API BOOL WINAPI PostMessageA(HWND, UINT, WPARAM, LPARAM);
API DWORD WINAPI MsgWaitForMultipleObjects(DWORD, const HANDLE *, BOOL, DWORD, DWORD);
API DWORD WINAPI GetQueueStatus(UINT);
/* Native MSG includes lPrivate after the public fields in this SDK. */
typedef struct { MSG message; DWORD private_value; } NATIVE_MSG;
static HANDLE started, create_queue, queue_ready, done;
static void value(const char *label, DWORD code) {
    static const char hex[] = "0123456789abcdef";
    char text[128]; DWORD n = 0, i, written;
    while (*label) text[n++] = *label++;
    text[n++] = '='; text[n++] = '0'; text[n++] = 'x';
    for (i = 0; i < 8; ++i) text[n++] = hex[(code >> (28 - i * 4)) & 15];
    text[n++] = '\r'; text[n++] = '\n';
    WriteFile(GetStdHandle((DWORD)-11), text, n, &written, NULL);
}
static DWORD WINAPI worker(void *unused) {
    NATIVE_MSG out = {0}; (void)unused;
    SetEvent(started);
    if (WaitForSingleObject(create_queue, 3000)) return 10;
    PeekMessageA(&out.message, 0, 0x400, 0x400, 0);
    SetEvent(queue_ready);
    if (WaitForSingleObject(done, 3000)) return 11;
    return 0;
}
static void take(const char *label, UINT min, UINT max, BOOL get) {
    NATIVE_MSG out = {0};
    BOOL result = get ? GetMessageA(&out.message, 0, min, max)
                      : PeekMessageA(&out.message, 0, min, max, 1);
    value(label, result ? out.message.message : 0);
    if (result) value("retrieved.wparam", out.message.wparam);
}
void mainCRTStartup(void) {
    DWORD tid, self = GetCurrentThreadId();
    HANDLE thread;
    NATIVE_MSG out = {0};
    started = CreateEventA(NULL, TRUE, FALSE, NULL);
    create_queue = CreateEventA(NULL, TRUE, FALSE, NULL);
    queue_ready = CreateEventA(NULL, TRUE, FALSE, NULL);
    done = CreateEventA(NULL, TRUE, FALSE, NULL);
    thread = CreateThread(NULL, 0, worker, NULL, 0, &tid);
    if (!thread || WaitForSingleObject(started, 3000)) ExitProcess(1);
    SetLastError(0);
    value("Post.before_target_queue", PostThreadMessageA(tid, 0x506, 1, 0));
    value("Post.before_target_queue.last_error", GetLastError());
    SetEvent(create_queue);
    if (WaitForSingleObject(queue_ready, 3000)) ExitProcess(2);
    value("Post.after_target_peek", PostThreadMessageA(tid, 0x506, 2, 0));
    SetEvent(done);
    if (WaitForSingleObject(thread, 3000)) ExitProcess(3);

    PeekMessageA(&out.message, 0, 0, 0, 1);
    PostThreadMessageA(self, 0x506, 10, 0);
    value("MsgWait.new_post", MsgWaitForMultipleObjects(0, NULL, FALSE, 0, 8));
    value("MsgWait.repeat_before_peek", MsgWaitForMultipleObjects(0, NULL, FALSE, 0, 8));
    value("Peek.no_remove", PeekMessageA(&out.message, 0, 0, 0, 0));
    value("MsgWait.after_no_remove", MsgWaitForMultipleObjects(0, NULL, FALSE, 0, 8));
    value("MsgWait.allpost_after_no_remove", MsgWaitForMultipleObjects(0, NULL, FALSE, 0, 0x100));
    PostThreadMessageA(self, 0x513, 11, 0);
    value("MsgWait.new_post_after_old", MsgWaitForMultipleObjects(0, NULL, FALSE, 0, 8));
    value("Peek.filtered_miss", PeekMessageA(&out.message, 0, 0x599, 0x599, 0));
    value("MsgWait.after_filtered_miss", MsgWaitForMultipleObjects(0, NULL, FALSE, 0, 8));
    value("MsgWait.allpost_after_filtered_miss", MsgWaitForMultipleObjects(0, NULL, FALSE, 0, 0x100));
    value("GetQueueStatus.after_filtered_miss", GetQueueStatus(0x108));
    value("GetQueueStatus.again", GetQueueStatus(0x108));
    value("MsgWait.allpost_after_queue_status", MsgWaitForMultipleObjects(0, NULL, FALSE, 0, 0x100));
    while (PeekMessageA(&out.message, 0, 0, 0, 1)) {}

    PostThreadMessageA(self, 0x501, 1, 0);
    PostThreadMessageA(self, 0x506, 2, 0);
    PostThreadMessageA(self, 0x513, 3, 0);
    PostThreadMessageA(self, 0x506, 4, 0);
    PostThreadMessageA(self, 0x513, 5, 0);
    take("Peek.only_506", 0x506, 0x506, FALSE);
    take("Get.unfiltered", 0, 0, TRUE);
    take("Get.only_513", 0x513, 0x513, TRUE);
    take("Peek.unfiltered_1", 0, 0, FALSE);
    take("Peek.unfiltered_2", 0, 0, FALSE);
    value("Peek.empty", PeekMessageA(&out.message, 0, 0, 0, 1));
    {
        UINT messages[] = { 0x100, 0x102, 0x200, 0xf, 0x113, 0x506 };
        DWORD i;
        for (i = 0; i < 6; ++i) {
            value("Post.category.message", messages[i]);
            PostThreadMessageA(self, messages[i], 0, 0);
            value("PostThread.category.status", GetQueueStatus(0x13f));
            PeekMessageA(&out.message, 0, 0, 0, 1);
            PostMessageA(0, messages[i], 0, 0);
            value("PostMessage.category.status", GetQueueStatus(0x13f));
            PeekMessageA(&out.message, 0, 0, 0, 1);
        }
    }
    {
        WNDCLASSA wc = {0}; HWND hwnd;
        wc.procedure = DefWindowProcA; wc.name = "QueueTranslateProbe";
        wc.instance = GetModuleHandleA(NULL); RegisterClassA(&wc);
        hwnd = CreateWindowExA(0, wc.name, "", 0, 0, 0, 32, 32, 0, 0, wc.instance, NULL);
        while (PeekMessageA(&out.message, 0, 0, 0, 1)) {}
        PostMessageA(hwnd, 0x100, 0x41, 0x001e0001);
        PeekMessageA(&out.message, 0, 0, 0, 1);
        value("Translate.return", TranslateMessage(&out.message));
        value("Translate.character.status", GetQueueStatus(0x13f));
        value("Translate.character.peek", PeekMessageA(&out.message, 0, 0, 0, 1));
        value("Translate.character.message", out.message.message);
        DestroyWindow(hwnd);
    }
    {
        HANDLE events[9]; DWORD i;
        for (i = 0; i < 9; ++i) events[i] = CreateEventA(NULL, FALSE, FALSE, NULL);
        PostThreadMessageA(self, 0x506, 20, 0);
        value("SetEvent.seventh", SetEvent(events[7]));
        value("SetEvent.second", SetEvent(events[2]));
        value("MsgWait.nine.new_queue_priority_1", MsgWaitForMultipleObjects(9, events, FALSE, 0, 8));
        value("MsgWait.nine.new_queue_priority_2", MsgWaitForMultipleObjects(9, events, FALSE, 0, 8));
        value("MsgWait.nine.new_queue_priority_3", MsgWaitForMultipleObjects(9, events, FALSE, 0, 8));
        PeekMessageA(&out.message, 0, 0, 0, 0);
        value("MsgWait.nine.old_queue", MsgWaitForMultipleObjects(9, events, FALSE, 0, 8));
        value("MsgWait.nine.old_queue_next_event", MsgWaitForMultipleObjects(9, events, FALSE, 0, 8));
        value("MsgWait.nine.old_queue_timeout", MsgWaitForMultipleObjects(9, events, FALSE, 0, 8));
        for (i = 0; i < 9; ++i) CloseHandle(events[i]);
    }
    CloseHandle(thread); CloseHandle(started); CloseHandle(create_queue);
    CloseHandle(queue_ready); CloseHandle(done);
    ExitProcess(0);
}
