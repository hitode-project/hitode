/* Authored Wine surface damage probe; the host checkpoints inspect publications.
 * Build: i686-w64-mingw32-gcc -O1 -o frame_damage.exe frame_damage.c -lgdi32 */
#include <windows.h>
#include <stdio.h>

static int phase;
static DWORD mouse_point = 0xffffffff;
static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message == WM_MOUSEMOVE) mouse_point = (DWORD)lparam;
    if (message == WM_PAINT)
    {
        PAINTSTRUCT paint;
        HDC dc = BeginPaint(window, &paint);
        HBRUSH brush = CreateSolidBrush(phase == 2 ? RGB(255, 0, 0) : RGB(0, 0, 255));
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

int main(void)
{
    WNDCLASSW cls = {0};
    RECT part = {0, 5, 16, 8};
    void (__cdecl *checkpoint)(int) = (void *)GetProcAddress(LoadLibraryA("frame_probe.dll"), "checkpoint");
    if (!checkpoint) return 2;
    cls.lpfnWndProc = procedure;
    cls.hInstance = GetModuleHandleW(NULL);
    cls.lpszClassName = L"damage";
    if (!RegisterClassW(&cls)) return 3;
    HWND window = CreateWindowExW(0, cls.lpszClassName, L"damage", WS_POPUP | WS_VISIBLE,
                                  0, 0, 128, 64, NULL, NULL, cls.hInstance, NULL);
    if (!window) return 4;
    SetForegroundWindow(window);
    SetFocus(window);
    UpdateWindow(window);
    flush();
    checkpoint(0);
    InvalidateRect(window, NULL, FALSE);
    UpdateWindow(window);
    flush();
    checkpoint(1);
    phase = 2;
    InvalidateRect(window, &part, FALSE);
    UpdateWindow(window);
    flush();
    checkpoint(2);
    InvalidateRect(window, &part, FALSE);
    UpdateWindow(window);
    flush();
    checkpoint(3);
    SetWindowPos(window, NULL, 31, 47, 0, 0,
                 SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW);
    InvalidateRect(window, &part, FALSE);
    UpdateWindow(window);
    flush();
    checkpoint(4);
    flush();
    printf("mouse=%d,%d\n", (short)mouse_point, (short)(mouse_point >> 16));
    DestroyWindow(window);
    return mouse_point != (17u << 16 | 13u);
}
