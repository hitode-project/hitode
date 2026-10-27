/* The text probe: GDI draws a TrueType string into a DIB section through
 * win32u's FreeType renderer. It exits with 0 only when the face is the
 * one asked for, the string has a width and its pixels were drawn.
 * Build: i686-w64-mingw32-gcc -O1 -o text.exe text.c -lgdi32 */
#include <windows.h>
#include <stdio.h>

#define WIDTH 64
#define HEIGHT 32

int main(void)
{
    BITMAPINFO info = {{sizeof(BITMAPINFOHEADER), WIDTH, -HEIGHT, 1, 32, BI_RGB}};
    static const WCHAR text[] = L"Hi";
    DWORD *pixels;
    HDC dc = CreateCompatibleDC(NULL);
    HBITMAP bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, (void **)&pixels, NULL, 0);
    HFONT font = CreateFontW(-16, 0, 0, 0, FW_NORMAL, 0, 0, 0, ANSI_CHARSET, 0, 0,
                             NONANTIALIASED_QUALITY, 0, L"Tahoma");
    WCHAR face[LF_FACESIZE] = {0};
    SIZE size = {0};
    int drawn, inked = 0, x, y;

    SelectObject(dc, bitmap);
    SelectObject(dc, font);
    SetTextColor(dc, RGB(255, 255, 255));
    SetBkMode(dc, TRANSPARENT);
    GetTextFaceW(dc, LF_FACESIZE, face);
    GetTextExtentPoint32W(dc, text, 2, &size);
    drawn = TextOutW(dc, 4, 4, text, 2);
    GdiFlush();
    for (y = 0; y < HEIGHT; y++)
        for (x = 0; x < WIDTH; x++)
            if (pixels[y * WIDTH + x] == 0xffffff) inked++;

    printf("face=%ls width=%ld height=%ld drawn=%d inked=%d\n", face, size.cx, size.cy, drawn,
           inked);
    return !wcscmp(face, L"Tahoma") && size.cx > 0 && drawn && inked > 0 ? 0 : 1;
}
