/* Authored probe: a worker thread paints a window via GDI BitBlt on a cached
 * HDC and waits on an event between frames while the UI thread stays blocked
 * in GetMessageA. Every frame must publish on the worker's wait rather than
 * waiting for win32u's 50 ms redraw gate (WINE-GFX-7).
 * Build: i686-w64-mingw32-gcc -O1 -o gdi_worker_flush.exe gdi_worker_flush.c -lgdi32 */
#include <windows.h>

static HWND g_hwnd;
static HANDLE g_stop;

static DWORD WINAPI render_thread(LPVOID param)
{
    (void)param;
    HDC dst = GetDC(g_hwnd);
    HDC src = CreateCompatibleDC(dst);
    HBITMAP bmp = CreateCompatibleBitmap(dst, 64, 64);
    HGDIOBJ old = SelectObject(src, bmp);
    RECT rect = {0, 0, 64, 64};
    /* Six distinct colors spaced 10 ms apart (< 50 ms each, 60 ms total):
     * without wait-triggered window_surface flushing, most frames are dropped
     * and the final color never appears while GetMessageA stays idle. */
    COLORREF colors[6] = {
        RGB(255, 0, 0),
        RGB(0, 255, 0),
        RGB(0, 0, 255),
        RGB(255, 255, 0),
        RGB(0, 255, 255),
        RGB(255, 0, 255),
    };
    for (int i = 0; i < 6; ++i)
    {
        HBRUSH brush = CreateSolidBrush(colors[i]);
        FillRect(src, &rect, brush);
        DeleteObject(brush);
        BitBlt(dst, 0, 0, 64, 64, src, 0, 0, SRCCOPY);
        WaitForSingleObject(g_stop, 10);
    }
    SelectObject(src, old);
    DeleteObject(bmp);
    DeleteDC(src);
    ReleaseDC(g_hwnd, dst);
    /* Stay blocked so the host observes the final published frame. */
    WaitForSingleObject(g_stop, INFINITE);
    return 0;
}

int main(void)
{
    WNDCLASSA cls = {0};
    cls.lpfnWndProc = DefWindowProcA;
    cls.hInstance = GetModuleHandleA(NULL);
    cls.lpszClassName = "gdi_worker_flush";
    if (!RegisterClassA(&cls)) return 2;
    g_hwnd = CreateWindowExA(0, cls.lpszClassName, "gdi_worker_flush", WS_POPUP | WS_VISIBLE,
                             0, 0, 64, 64, NULL, NULL, cls.hInstance, NULL);
    if (!g_hwnd) return 3;
    g_stop = CreateEventA(NULL, TRUE, FALSE, NULL);
    if (!g_stop) return 4;
    HANDLE thread = CreateThread(NULL, 0, render_thread, NULL, 0, NULL);
    if (!thread) return 5;
    CloseHandle(thread);
    MSG msg;
    while (GetMessageA(&msg, NULL, 0, 0) > 0)
    {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
    return 0;
}
