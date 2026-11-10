/* Window planes probe: as toolkits do, a hidden application window owns
 * every window. A smaller window shown first does not keep the frame from
 * the main window, popups show over the main window's frame without
 * replacing it, one shown off the main window moves into it, a larger modal
 * window that disables the main window shows over it too, and host mouse
 * points stay in the main window's coordinates.
 * Build: i686-w64-mingw32-gcc -O1 -o window_planes.exe window_planes.c -lgdi32 */
#include <windows.h>

static DWORD popup_point = 0xffffffff;
static HWND clicked;

static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message == WM_LBUTTONDOWN)
    {
        popup_point = (DWORD)lparam;
        clicked = window;
    }
    if (message == WM_PAINT)
    {
        PAINTSTRUCT paint;
        HDC dc = BeginPaint(window, &paint);
        HBRUSH brush = CreateSolidBrush((COLORREF)GetWindowLongPtrW(window, GWLP_USERDATA));
        FillRect(dc, &paint.rcPaint, brush);
        DeleteObject(brush);
        EndPaint(window, &paint);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

static void flush(void)
{
    MSG message;
    for (int i = 0; i < 5; ++i)
    {
        while (PeekMessageW(&message, NULL, 0, 0, PM_REMOVE)) DispatchMessageW(&message);
        Sleep(20);
    }
}

static HWND show(HWND owner, int x, int y, int width, int height, COLORREF color, int activate)
{
    HWND window = CreateWindowExW(0, L"planes", L"planes", WS_POPUP, x, y, width, height,
                                  owner, NULL, GetModuleHandleW(NULL), NULL);
    SetWindowLongPtrW(window, GWLP_USERDATA, color);
    ShowWindow(window, activate ? SW_SHOW : SW_SHOWNOACTIVATE);
    UpdateWindow(window);
    return window;
}

int main(void)
{
    WNDCLASSW cls = {0};
    RECT moved;
    void (__cdecl *checkpoint)(int) = (void *)GetProcAddress(LoadLibraryA("planes_probe.dll"), "checkpoint");
    if (!checkpoint) return 2;
    cls.lpfnWndProc = procedure;
    cls.hInstance = GetModuleHandleW(NULL);
    cls.lpszClassName = L"planes";
    if (!RegisterClassW(&cls)) return 3;
    HWND application = CreateWindowExW(0, L"planes", L"application", WS_POPUP, 0, 0, 0, 0,
                                       NULL, NULL, GetModuleHandleW(NULL), NULL);
    HWND splash = show(application, 100, 100, 64, 32, RGB(128, 128, 128), 1);
    flush();
    HWND main = show(application, 100, 100, 128, 64, RGB(0, 0, 255), 1);
    SetForegroundWindow(main);
    SetFocus(main);
    flush();
    checkpoint(0);
    HWND popup = show(application, 116, 108, 32, 16, RGB(255, 0, 0), 0);
    flush();
    checkpoint(1);
    flush();
    HWND outside = show(main, 2000, 1300, 16, 16, RGB(0, 255, 0), 0);
    flush();
    checkpoint(2);
    DestroyWindow(popup);
    flush();
    checkpoint(3);
    EnableWindow(main, FALSE);
    HWND modal = show(application, 90, 90, 192, 96, RGB(255, 255, 0), 1);
    flush();
    checkpoint(4);
    EnableWindow(main, TRUE);
    DestroyWindow(modal);
    GetWindowRect(outside, &moved);
    DestroyWindow(outside);
    DestroyWindow(main);
    DestroyWindow(splash);
    DestroyWindow(application);
    if (moved.left != 212 || moved.top != 148) return 4;
    /* The host click at (20, 10) of the main window is (4, 2) in the popup. */
    return clicked != popup || popup_point != (2u << 16 | 4u) ? 5 : 0;
}
