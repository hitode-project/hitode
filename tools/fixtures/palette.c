#define WIN32_LEAN_AND_MEAN
#include <windows.h>

static DWORD words[512];
static UINT used;
static struct { WORD version, count; PALETTEENTRY entries[65535]; } input;
static PALETTEENTRY output[24];
static void record(DWORD value) { words[used++] = value; }
static void snapshot(HPALETTE palette, UINT start, UINT count) {
    UINT i;
    for (i = 0; i < 24; ++i) {
        output[i].peRed = output[i].peGreen = output[i].peBlue = output[i].peFlags = 0xcc;
    }
    record(GetPaletteEntries(palette, start, count, output));
    for (i = 0; i < 24; ++i) {
        record(output[i].peRed | ((DWORD)output[i].peGreen << 8) | ((DWORD)output[i].peBlue << 16) | ((DWORD)output[i].peFlags << 24));
    }
}
void mainCRTStartup(void) {
    HPALETTE palette, stock;
    HDC dc, other;
    WORD count;
    DWORD written;
    HANDLE file;
    PALETTEENTRY replacement[2] = {{1,2,3,0xff},{4,5,6,0x08}};
    input.version = 0x300; input.count = 4;
    input.entries[0].peRed = 0;
    input.entries[1].peRed = input.entries[1].peGreen = input.entries[1].peBlue = 255;
    input.entries[2].peRed = 255; input.entries[2].peFlags = PC_RESERVED;
    input.entries[3].peGreen = 255; input.entries[3].peFlags = PC_NOCOLLAPSE;
    palette = CreatePalette((LOGPALETTE*)&input);
    record(palette != NULL);
    record(GetPaletteEntries(palette, 0, 0, NULL));
    record(GetPaletteEntries(palette, ~0U, 42, NULL));
    snapshot(palette, 0, 4);
    snapshot(palette, 3, 20);
    snapshot(palette, 4, 1);
    record(GetNearestPaletteIndex(palette, RGB(0,0,0)));
    record(GetNearestPaletteIndex(palette, RGB(255,255,255)));
    record(GetNearestPaletteIndex(palette, RGB(250,10,0)));
    record(GetNearestPaletteIndex(palette, RGB(0,250,10)));
    record(GetNearestPaletteIndex(palette, PALETTEINDEX(2)));
    record(GetNearestPaletteIndex(palette, PALETTEINDEX(4)));
    count = 0xcccc; record(GetObjectA(palette, 0, NULL));
    record(GetObjectA(palette, 1, &count)); record(count);
    record(GetObjectA(palette, 2, &count)); record(count);
    record(SetPaletteEntries(palette, 3, 2, replacement));
    snapshot(palette, 0, 4);
    record(SetPaletteEntries(palette, 4, 1, replacement));
    record(ResizePalette(palette, 6)); snapshot(palette, 0, 6);
    record(ResizePalette(palette, 2)); snapshot(palette, 0, 6);
    record(ResizePalette(palette, 0)); record(ResizePalette(palette, 1025));
    stock = (HPALETTE)GetStockObject(DEFAULT_PALETTE);
    record(GetPaletteEntries(stock, 0, 0, NULL)); snapshot(stock, 0, 20);
    record(SetPaletteEntries(stock, 0, 1, replacement)); record(ResizePalette(stock, 2));
    dc = CreateCompatibleDC(NULL); other = CreateCompatibleDC(NULL);
    record(SelectPalette(dc, palette, FALSE) == stock);
    record(SelectPalette(other, palette, TRUE) == stock);
    record(GetCurrentObject(dc, OBJ_PAL) == palette);
    record(RealizePalette(dc));
    record(SelectObject(dc, palette) == NULL);
    record(SelectObject(dc, stock) == NULL);
    /* The drawing provider and logical GDI metadata share font/bitmap selections. */
    {
        HGDIOBJ oldfont = GetCurrentObject(dc, OBJ_FONT), font = GetStockObject(ANSI_FIXED_FONT);
        BITMAPINFO info = {0}; void *pixels;
        HBITMAP bitmap; HGDIOBJ oldbitmap = GetCurrentObject(dc, OBJ_BITMAP);
        record(oldfont != NULL); record(SelectObject(dc, font) == oldfont);
        record(GetCurrentObject(dc, OBJ_FONT) == font); record(SelectObject(dc, oldfont) == font);
        info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER); info.bmiHeader.biWidth = 4;
        info.bmiHeader.biHeight = -4; info.bmiHeader.biPlanes = 1; info.bmiHeader.biBitCount = 32;
        bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &pixels, NULL, 0);
        record(bitmap != NULL); record(oldbitmap != NULL);
        record(SelectObject(dc, bitmap) == oldbitmap); record(GetCurrentObject(dc, OBJ_BITMAP) == bitmap);
        record(SelectObject(dc, oldbitmap) == bitmap); record(DeleteObject(bitmap));
    }
    record(DeleteObject(palette));
    record(SelectPalette(dc, stock, TRUE) == palette);
    record(DeleteObject(palette));
    record(SelectPalette(other, stock, TRUE) == palette);
    record(DeleteObject(palette));
    record(DeleteObject(palette));
    record(GetPaletteEntries(palette, 0, 0, NULL));
    record(GetNearestPaletteIndex(palette, 0));
    record(RealizePalette(NULL));
    record(SelectPalette(dc, NULL, FALSE) == NULL);
    record(DeleteDC(dc)); record(DeleteDC(other));
    input.version = 0; record(CreatePalette((LOGPALETTE*)&input) == NULL);
    input.version = 0x300; input.count = 0; record(CreatePalette((LOGPALETTE*)&input) == NULL);
    input.count = 65535; palette = CreatePalette((LOGPALETTE*)&input);
    record(palette != NULL); record(GetPaletteEntries(palette, 0, 0, NULL));
    record(ResizePalette(palette, 1024)); record(DeleteObject(palette));
    file = CreateFileA("palette.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, words, used * sizeof(DWORD), &written, NULL) || written != used * sizeof(DWORD)) ExitProcess(1);
    CloseHandle(file); ExitProcess(0);
}
