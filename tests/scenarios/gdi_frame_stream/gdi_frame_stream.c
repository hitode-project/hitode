/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 gdi_frame_stream.c -o gdi_frame_stream.exe -lgdi32 -luser32 */
/* A window repaints its whole client area from a DIB as fast as GDI
 * accepts it. Every frame takes a software slot, so publication must keep
 * retiring slots without further commands from the guest.
 * gdi-frame-stream.bin holds the number of frames drawn. Exit status: 0
 * when all frames were drawn within the time limit, 30 when they were not. */
#include <windows.h>

enum { WIDTH = 640, HEIGHT = 480, FRAMES = 600, LIMIT_MS = 30000 };

static DWORD pixels[WIDTH * HEIGHT];

int main(void) {
    BITMAPINFO info = {{sizeof(BITMAPINFOHEADER), WIDTH, -HEIGHT, 1, 32, BI_RGB}};
    HWND window = CreateWindowExA(0, "STATIC", "frames", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0,
                                  WIDTH, HEIGHT, NULL, NULL, GetModuleHandleA(NULL), NULL);
    DWORD start = GetTickCount(), frame, i, n;
    HANDLE out;
    MSG message;
    HDC dc;

    if (!window) return 3;
    dc = GetDC(window);
    for (frame = 0; frame < FRAMES && GetTickCount() - start < LIMIT_MS; ++frame) {
        for (i = 0; i < WIDTH * HEIGHT; ++i) pixels[i] = (i + frame * 0x010203u) * 0x9e3779b1u;
        StretchDIBits(dc, 0, 0, WIDTH, HEIGHT, 0, 0, WIDTH, HEIGHT, pixels, &info, DIB_RGB_COLORS,
                      SRCCOPY);
        GdiFlush();
        while (PeekMessageA(&message, NULL, 0, 0, PM_REMOVE)) DispatchMessageA(&message);
    }
    ReleaseDC(window, dc);
    out = CreateFileA("gdi-frame-stream.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS,
                      FILE_ATTRIBUTE_NORMAL, NULL);
    if (out == INVALID_HANDLE_VALUE || !WriteFile(out, &frame, 4, &n, NULL)) return 4;
    CloseHandle(out);
    return frame == FRAMES ? 0 : 30;
}
