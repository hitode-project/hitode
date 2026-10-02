#include "win32_min.h"

static volatile DWORD spins;
static LRESULT WINAPI window_proc(HWND hwnd, UINT message, WPARAM wp, LPARAM lp) {
    if (message == WM_CLOSE) {
        /* Model a guest that never returns from its close handler. The frontend
           must enforce its two-second close grace through the CPU budget. */
        for (;;) ++spins;
    }
    return DefWindowProcA(hwnd, message, wp, lp);
}
void mainCRTStartup(void) {
    WNDCLASSA klass = {0};
    MSG message;
    HWND window;
    klass.procedure = window_proc;
    klass.instance = GetModuleHandleA(NULL);
    klass.name = "AdvCloseFixture";
    if (!RegisterClassA(&klass)) ExitProcess(40);
    window = CreateWindowExA(0, klass.name, "Guest close budget fixture", 0x10cf0000u,
        0, 0, 336, 279, 0, 0, klass.instance, NULL);
    if (!window) ExitProcess(41);
    while (GetMessageA(&message, 0, 0, 0) > 0) {
        TranslateMessage(&message); DispatchMessageA(&message);
    }
    ExitProcess(0);
}
