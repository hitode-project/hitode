/* Offscreen DIB primitives. Build with -lgdi32 -lmsimg32. */
#include <windows.h>
#include <stdio.h>
#include <stdint.h>

static uint32_t hash(const DWORD *pixels, unsigned count)
{
    uint32_t value = 2166136261u;
    unsigned i, c;
    for (i = 0; i < count; i++)
        for (c = 0; c < 4; c++) value = (value ^ ((pixels[i] >> (c * 8)) & 255)) * 16777619u;
    return value;
}

int main(void)
{
    BITMAPINFO info = {{sizeof(BITMAPINFOHEADER), 64, -32, 1, 32, BI_RGB}};
    HDC dst = CreateCompatibleDC(NULL), src = CreateCompatibleDC(NULL);
    DWORD *d, *s;
    HBITMAP db = CreateDIBSection(dst, &info, DIB_RGB_COLORS, (void **)&d, NULL, 0);
    HBITMAP sb = CreateDIBSection(src, &info, DIB_RGB_COLORS, (void **)&s, NULL, 0);
    HGDIOBJ old_d, old_s;
    BLENDFUNCTION blend = {AC_SRC_OVER, 0, 127, AC_SRC_ALPHA};
    RECT rect = {2, 2, 40, 20};
    HFONT font;
    HGDIOBJ old_font;
    unsigned i, pass;
    BOOL blended, text;
    uint32_t blended_hash;
    if (!dst || !src || !db || !sb) return 1;
    old_d = SelectObject(dst, db);
    old_s = SelectObject(src, sb);
    for (i = 0; i < 64 * 32; i++) {
        d[i] = 0x19345678u + i * 0x00010203u;
        s[i] = 0x80000000u | ((i * 17 & 127) << 16) | ((i * 29 & 127) << 8) | (i * 37 & 127);
    }
    blended = TRUE;
    for (pass = 0; pass < 8; pass++)
        blended &= AlphaBlend(dst, 3, 2, 40, 20, src, 2, 1, 40, 20, blend);
    GdiFlush();
    blended_hash = hash(d, 64 * 32);
    if (AddFontResourceExW(L"C:\\private.ttf", FR_PRIVATE, NULL) != 1) return 2;
    font = CreateFontW(-16, 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE, DEFAULT_CHARSET,
                       OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY,
                       DEFAULT_PITCH, L"ADV Resource Fixture");
    old_font = SelectObject(dst, font);
    SetTextColor(dst, RGB(230, 100, 10));
    SetBkColor(dst, RGB(30, 60, 90));
    text = TRUE;
    for (pass = 0; pass < 8; pass++)
        text &= ExtTextOutW(dst, 4, 4, ETO_OPAQUE | ETO_CLIPPED, &rect, L"BB", 2, NULL);
    GdiFlush();
    printf("blend=%d text=%d alpha=%08lx pixels=%08lx\n", blended, text,
           (unsigned long)blended_hash, (unsigned long)hash(d, 64 * 32));
    SelectObject(dst, old_font);
    DeleteObject(font);
    SelectObject(dst, old_d);
    SelectObject(src, old_s);
    DeleteObject(db);
    DeleteObject(sb);
    DeleteDC(dst);
    DeleteDC(src);
    return !blended || !text;
}
