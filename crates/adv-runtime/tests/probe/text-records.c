/* Authored text record oracle (WINE-UNIX-8): no sample input.
 * Measures without drawing, then draws known lines into a DIB through
 * TextOutW, Shift_JIS ExtTextOutA, a two-line DrawTextW and glyph indices,
 * and fetches one glyph. It then moves that text: BitBlt to a window, after
 * an unrelated blit that holds no text, AlphaBlend to a second DIB and
 * GetDIBits to a 24-bit buffer, and draws one line straight onto the window.
 * Writes text-records.bin (see `struct out`) and waits for the host.
 * Build: i686-w64-mingw32-gcc -O2 -Wall text-records.c -o text-records.exe -lgdi32 -luser32 */
#include <windows.h>

static BYTE glyph[8192];
static BYTE converted[160 * 96 * 3];

struct out
{
    DWORD glyph, glyph_bytes;
    DWORD first_index, second_index;
    DWORD hwnd, converted;
};

static HDC dib_dc(int width, int height)
{
    BITMAPINFO info = {0};
    void *pixels;
    HDC dc = CreateCompatibleDC(0);
    info.bmiHeader.biSize = sizeof info.bmiHeader;
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = -height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    HBITMAP bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &pixels, 0, 0);
    if (!dc || !bitmap) ExitProcess(1);
    SelectObject(dc, bitmap);
    return dc;
}

int main(void)
{
    struct out out;
    HDC dc = dib_dc(160, 96);
    HFONT font = CreateFontW(-16, 0, 0, 0, 400, 0, 0, 0, SHIFTJIS_CHARSET, 0, 0,
                             ANTIALIASED_QUALITY, 0, L"MS Gothic");
    if (!font) ExitProcess(1);
    SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT);

    SIZE size;
    if (!GetTextExtentPoint32W(dc, L"\x3042Z", 2, &size)) ExitProcess(1);
    if (!TextOutW(dc, 2, 2, L"\x3042" L"A", 2)) ExitProcess(1);
    if (!ExtTextOutA(dc, 2, 20, 0, 0, "\x82\xa0" "B", 3, 0)) ExitProcess(1);
    RECT rect = {2, 38, 158, 94};
    if (!DrawTextW(dc, L"C\r\nD", -1, &rect, 0)) ExitProcess(1);
    WORD indices[2];
    if (GetGlyphIndicesW(dc, L"AB", 2, indices, 0) != 2) ExitProcess(1);
    if (!ExtTextOutW(dc, 80, 2, ETO_GLYPH_INDEX, 0, (LPCWSTR)indices, 2, 0)) ExitProcess(1);
    GLYPHMETRICS metrics;
    MAT2 identity = {{0, 1}, {0, 0}, {0, 0}, {0, 1}};
    DWORD bytes = GetGlyphOutlineW(dc, 0x3042, GGO_GRAY8_BITMAP, &metrics, sizeof glyph, glyph,
                                   &identity);
    if (bytes == GDI_ERROR || !bytes) ExitProcess(1);

    WNDCLASSA cls = {0};
    cls.lpfnWndProc = DefWindowProcA;
    cls.hInstance = GetModuleHandleA(NULL);
    cls.lpszClassName = "text-records";
    if (!RegisterClassA(&cls)) ExitProcess(3);
    HWND hwnd = CreateWindowExA(0, cls.lpszClassName, "text-records", WS_POPUP | WS_VISIBLE, 0, 0,
                                200, 120, NULL, NULL, cls.hInstance, NULL);
    if (!hwnd) ExitProcess(3);
    HDC window = GetDC(hwnd);
    HDC plain = dib_dc(32, 16);
    if (!window || !BitBlt(window, 0, 100, 32, 16, plain, 0, 0, SRCCOPY)) ExitProcess(4);
    if (!BitBlt(window, 10, 4, 160, 96, dc, 0, 0, SRCCOPY)) ExitProcess(4);
    HDC blended = dib_dc(64, 32);
    BLENDFUNCTION half = {AC_SRC_OVER, 0, 128, 0};
    if (!GdiAlphaBlend(blended, 0, 0, 64, 32, dc, 2, 2, 64, 32, half)) ExitProcess(5);
    BITMAPINFO info = {0};
    info.bmiHeader.biSize = sizeof info.bmiHeader;
    info.bmiHeader.biWidth = 160;
    info.bmiHeader.biHeight = -96;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 24;
    HBITMAP bitmap = GetCurrentObject(dc, OBJ_BITMAP);
    if (GetDIBits(dc, bitmap, 0, 96, converted, &info, DIB_RGB_COLORS) != 96) ExitProcess(6);
    SelectObject(window, font);
    SetBkMode(window, TRANSPARENT);
    if (!TextOutW(window, 4, 104, L"E", 1)) ExitProcess(7);
    GdiFlush();

    out.glyph = (DWORD)(UINT_PTR)glyph;
    out.glyph_bytes = bytes;
    out.first_index = indices[0];
    out.second_index = indices[1];
    out.hwnd = (DWORD)(UINT_PTR)hwnd;
    out.converted = (DWORD)(UINT_PTR)converted;
    HANDLE file = CreateFileA("text-records.bin", GENERIC_WRITE, 0, 0, CREATE_ALWAYS, 0, 0);
    DWORD written;
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, &out, sizeof out, &written, 0) ||
        written != sizeof out)
        ExitProcess(2);
    CloseHandle(file);
    /* Keep the window, and so its surface, until the host has looked. */
    Sleep(INFINITE);
    return 0;
}
