#include "win32_min.h"
static HWND window;
static HDC memory_dc;
static DWORD *pixels;
static DWORD colour = 0xff112233u;
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
    if (message == WM_KEYDOWN) { colour = 0xff33aa11u; draw(); return 0; }
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
    while (GetMessageA(&message, 0, 0, 0) > 0) { TranslateMessage(&message); DispatchMessageA(&message); }
    ExitProcess(0);
}
