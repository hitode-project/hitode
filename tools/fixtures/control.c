#include "win32_min.h"
API WORD WINAPI GetAsyncKeyState(int);
static HWND window;
static HDC memory_dc;
static DWORD *pixels;
static DWORD colour = 0xff112233u;
#ifdef COPY_MESSAGE
static MSG copied_message;
#endif
#ifdef COMPUTE_WORKER
static volatile DWORD ticks;
static DWORD WINAPI compute_worker(void *unused) {
    (void)unused;
    for (;;) ++ticks;
}
#endif
static void draw(void) {
    int i;
    HDC dc;
    if (!pixels) return;
    for (i = 0; i < 64 * 32; ++i) pixels[i] = colour;
    dc = GetDC(window);
    BitBlt(dc, 0, 0, 64, 32, memory_dc, 0, 0, SRCCOPY);
    GdiFlush(); ReleaseDC(window, dc);
}
static LRESULT WINAPI window_proc(HWND hwnd, UINT message, WPARAM wp, LPARAM lp) {
    if (message == WM_LBUTTONDOWN) { colour = 0xffaa3311u; draw(); return 0; }
    if (message == WM_KEYDOWN) {
        if (wp == 17) {
            if (!(GetAsyncKeyState(17) & 0x8000)) ExitProcess(53);
#ifdef LIVE_INPUT
            /* Deliberately keep the window procedure inside WM_KEYDOWN. The
               frontend must publish release without dispatching WM_KEYUP. */
            colour = 0xff33aa11u; draw();
            while (GetAsyncKeyState(17) & 0x8000) Sleep(0);
            colour = 0xff1133aau; draw(); return 0;
#else
            SetTimer(hwnd, 7, 10, NULL);
#endif
        }
        colour = 0xff33aa11u; draw(); return 0;
    }
    if (message == 0x113 && wp == 7) {
        /* Hardware release is visible before its queued WM_KEYUP, including
           when a compute context delays message dispatch. This matches Win32. */
        if (!(GetAsyncKeyState(17) & 0x8000)) {
            KillTimer(hwnd, 7);
            colour = 0xff1133aau;
        }
        draw(); return 0;
    }
    if (message == 0x101 && wp == 17) {
        KillTimer(hwnd, 7);
        if (GetAsyncKeyState(17) & 0x8000) ExitProcess(55);
        colour = 0xff1133aau; draw(); return 0;
    }
    if (message == WM_PAINT) {
        PAINTSTRUCT paint; BeginPaint(hwnd, &paint); EndPaint(hwnd, &paint); draw(); return 0;
    }
    if (message == WM_CLOSE) { DestroyWindow(hwnd); return 0; }
    if (message == WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcA(hwnd, message, wp, lp);
}
void mainCRTStartup(void) {
    WNDCLASSA klass = {0};
    BITMAPINFO info = {0};
    HBITMAP bitmap;
    MSG message;
    klass.procedure = window_proc; klass.instance = GetModuleHandleA(NULL); klass.name = "AdvControlFixture";
    if (!RegisterClassA(&klass)) ExitProcess(50);
    window = CreateWindowExA(0, klass.name, "stdin control fixture", 0x10cf0000u,
        0, 0, 80, 71, 0, 0, klass.instance, NULL);
    memory_dc = CreateCompatibleDC(0);
    info.header.size = sizeof(BITMAPINFOHEADER); info.header.width = 64; info.header.height = -32;
    info.header.planes = 1; info.header.bpp = 32;
    bitmap = CreateDIBSection(memory_dc, &info, 0, (void **)&pixels, 0, 0);
    if (!bitmap) ExitProcess(51);
    SelectObject(memory_dc, bitmap);
    ShowWindow(window, 5); UpdateWindow(window); draw();
#ifdef COMPUTE_WORKER
    if (!CreateThread(NULL, 0, compute_worker, NULL, 0, NULL)) ExitProcess(52);
#endif
    while (GetMessageA(&message, 0, 0, 0) > 0) {
#ifdef SELF_INPUT
        /* A polling engine may retrieve input and handle it without calling
           DispatchMessage. Diagnostic fences must not require that API. */
        if ((message.message == WM_KEYDOWN || message.message == 0x101) && message.wparam == 17) {
            colour = message.message == WM_KEYDOWN ? 0xff33aa11u : 0xff1133aau;
            draw(); continue;
        }
#endif
#ifdef COPY_MESSAGE
        /* The address differs from the original GetMessage output slot. */
        copied_message = message;
        TranslateMessage(&copied_message); DispatchMessageA(&copied_message);
#else
        TranslateMessage(&message); DispatchMessageA(&message);
#endif
    }
    ExitProcess(0);
}
