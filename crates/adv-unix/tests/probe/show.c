/* The display probe, for a look in a host window: GDI draws a TrueType
 * string into a DirectDraw surface through the surface's DC, and every
 * WM_PAINT blits the surface to the primary surface over the window. It
 * runs until the window is closed.
 * Build: i686-w64-mingw32-gcc -O1 -mwindows -o show.exe show.c -lddraw -ldxguid -lgdi32 -luser32 */
#define COBJMACROS
#include <windows.h>
#include <ddraw.h>

#define WIDTH 320
#define HEIGHT 120

static IDirectDraw7 *draw;
static IDirectDrawSurface7 *surface, *primary;

static void paint(HWND window)
{
    PAINTSTRUCT ps;
    RECT rect = {0, 0, WIDTH, HEIGHT};
    POINT origin = {0, 0};

    BeginPaint(window, &ps);
    ClientToScreen(window, &origin);
    OffsetRect(&rect, origin.x, origin.y);
    IDirectDrawSurface7_Blt(primary, &rect, surface, NULL, DDBLT_WAIT, NULL);
    EndPaint(window, &ps);
}

static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
{
    switch (message)
    {
    case WM_PAINT:
        paint(window);
        return 0;
    case WM_DESTROY:
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

int WINAPI WinMain(HINSTANCE instance, HINSTANCE previous, LPSTR command, int show)
{
    WNDCLASSW class = {0};
    DDSURFACEDESC2 desc = {sizeof(desc)};
    DDBLTFX fx = {sizeof(fx)};
    HFONT font;
    HWND window;
    MSG message;
    HDC dc;

    class.lpfnWndProc = procedure;
    class.hInstance = instance;
    class.lpszClassName = L"show";
    if (!RegisterClassW(&class)) return 2;
    window = CreateWindowExW(0, L"show", L"show", WS_POPUP | WS_VISIBLE, 0, 0, WIDTH, HEIGHT,
                             NULL, NULL, instance, NULL);
    if (!window) return 3;
    if (FAILED(DirectDrawCreateEx(NULL, (void **)&draw, &IID_IDirectDraw7, NULL))) return 4;
    IDirectDraw7_SetCooperativeLevel(draw, window, DDSCL_NORMAL);

    desc.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT;
    desc.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
    desc.dwWidth = WIDTH;
    desc.dwHeight = HEIGHT;
    if (FAILED(IDirectDraw7_CreateSurface(draw, &desc, &surface, NULL))) return 5;
    memset(&desc, 0, sizeof(desc));
    desc.dwSize = sizeof(desc);
    desc.dwFlags = DDSD_CAPS;
    desc.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
    if (FAILED(IDirectDraw7_CreateSurface(draw, &desc, &primary, NULL))) return 6;

    fx.dwFillColor = 0x0000ff;
    IDirectDrawSurface7_Blt(surface, NULL, NULL, NULL, DDBLT_COLORFILL | DDBLT_WAIT, &fx);
    font = CreateFontW(-22, 0, 0, 0, FW_BOLD, 0, 0, 0, ANSI_CHARSET, 0, 0,
                       ANTIALIASED_QUALITY, 0, L"Tahoma");
    if (FAILED(IDirectDrawSurface7_GetDC(surface, &dc))) return 7;
    SelectObject(dc, font);
    SetTextColor(dc, RGB(255, 255, 255));
    SetBkMode(dc, TRANSPARENT);
    TextOutW(dc, 16, 16, L"DirectDraw + Wine GDI", 21);
    SetTextColor(dc, RGB(255, 255, 0));
    TextOutW(dc, 16, 64, L"Hitode", 6);
    IDirectDrawSurface7_ReleaseDC(surface, dc);
    InvalidateRect(window, NULL, FALSE);

    while (GetMessageW(&message, NULL, 0, 0) > 0)
    {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    IDirectDrawSurface7_Release(primary);
    IDirectDrawSurface7_Release(surface);
    IDirectDraw7_Release(draw);
    return 0;
}
