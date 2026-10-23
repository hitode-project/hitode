/* The window probe: GDI paints a visible window in WM_PAINT, and the window
 * is read back through its DC. On Hitode the window surface is what the host
 * presents. It exits with 0 only when the text inked the window and the rest
 * kept the fill.
 * Build: i686-w64-mingw32-gcc -O1 -o window.exe window.c -lgdi32 */
#include <windows.h>
#include <stdio.h>

#define WIDTH 64
#define HEIGHT 32
#define WHITE 0xffffff
#define BLUE 0x0000ff

static int painted;

static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message == WM_PAINT)
    {
        PAINTSTRUCT paint;
        RECT rect = {0, 0, WIDTH, HEIGHT};
        HDC dc = BeginPaint(window, &paint);
        HBRUSH blue = CreateSolidBrush(RGB(0, 0, 255));
        HFONT font = CreateFontW(-16, 0, 0, 0, FW_NORMAL, 0, 0, 0, ANSI_CHARSET, 0, 0,
                                 NONANTIALIASED_QUALITY, 0, L"Tahoma");
        FillRect(dc, &rect, blue);
        SelectObject(dc, font);
        SetTextColor(dc, RGB(255, 255, 255));
        SetBkMode(dc, TRANSPARENT);
        painted = TextOutW(dc, 4, 4, L"Hi", 2);
        EndPaint(window, &paint);
        DeleteObject(font);
        DeleteObject(blue);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

int main(void)
{
    WNDCLASSW class = {0};
    BITMAPINFO info = {{sizeof(info.bmiHeader), WIDTH, -HEIGHT, 1, 32, BI_RGB}};
    HWND window;
    HDC dc, memory;
    HBITMAP bitmap;
    DWORD *bits, end;
    MSG message;
    int i, inked = 0, background = 0, other = 0;

    class.lpfnWndProc = procedure;
    class.hInstance = GetModuleHandleW(NULL);
    class.lpszClassName = L"probe";
    if (!RegisterClassW(&class)) return 2;
    window = CreateWindowExW(0, L"probe", L"probe", WS_POPUP | WS_VISIBLE, 0, 0, WIDTH, HEIGHT,
                             NULL, NULL, class.hInstance, NULL);
    if (!window) return 3;
    UpdateWindow(window);
    /* Let the idle message loop flush the window surface. */
    for (end = GetTickCount() + 100; GetTickCount() < end; Sleep(10))
        while (PeekMessageW(&message, NULL, 0, 0, PM_REMOVE)) DispatchMessageW(&message);

    dc = GetDC(window);
    memory = CreateCompatibleDC(dc);
    bitmap = CreateDIBSection(memory, &info, DIB_RGB_COLORS, (void **)&bits, NULL, 0);
    SelectObject(memory, bitmap);
    BitBlt(memory, 0, 0, WIDTH, HEIGHT, dc, 0, 0, SRCCOPY);
    for (i = 0; i < WIDTH * HEIGHT; i++)
    {
        DWORD pixel = bits[i] & 0xffffff;
        if (pixel == WHITE) inked++;
        else if (pixel == BLUE) background++;
        else other++;
    }
    DeleteDC(memory);
    DeleteObject(bitmap);
    ReleaseDC(window, dc);
    DestroyWindow(window);

    printf("painted=%d inked=%d background=%d other=%d\n", painted, inked, background, other);
    fflush(stdout);
    return !(painted && inked > 0 && background + inked == WIDTH * HEIGHT);
}
