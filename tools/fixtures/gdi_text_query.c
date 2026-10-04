#define WIN32_LEAN_AND_MEAN
#include <windows.h>

static DWORD words[2048], used;
static void record(DWORD value) { if (used >= 2048) ExitProcess(91); words[used++] = value; }
static void extent_a(HDC dc, const char *bytes, int count) {
    SIZE size = {(LONG)0xcccccccc, (LONG)0xdddddddd};
    record(GetTextExtentPoint32A(dc, bytes, count, &size));
    record((DWORD)size.cx); record((DWORD)size.cy);
}
static void extent_w(HDC dc, const WCHAR *units, int count) {
    SIZE size = {(LONG)0xcccccccc, (LONG)0xdddddddd};
    record(GetTextExtentPoint32W(dc, units, count, &size));
    record((DWORD)size.cx); record((DWORD)size.cy);
}
void mainCRTStartup(void) {
    static const char latin[] = {'W','i','A','V',0,'x','y'};
    static const char dbcs[] = {(char)0x82,(char)0xa0,(char)0x82,(char)0xa2,'A',0,'x'};
    static const WCHAR wide[] = {'W','i',0,'x',0x3042,0x3044,0xd83d,0xde00};
    static const char *faces[] = {"Arial", "Consolas", "MS Gothic"};
    HDC dc = CreateCompatibleDC(NULL);
    LOGFONTA input = {0}, output;
    HFONT font, old;
    HANDLE file;
    DWORD written;
    int face, charset, count, i;
    if (!dc) ExitProcess(1);
#ifdef PROBE_TEXTOUT
    /* The typed implementation explicitly rejects this unbacked default
       monochrome bitmap target, without reporting pixels or an ABI stub. */
    record(TextOutA(dc, 2, 3, "AV", 2));
#else
    for (face = 0; face < 3; ++face) for (charset = 0; charset < 3; ++charset) {
        input.lfHeight = -24; input.lfWeight = face == 1 ? 700 : 400;
        input.lfCharSet = charset == 0 ? ANSI_CHARSET : charset == 1 ? SHIFTJIS_CHARSET : GB2312_CHARSET;
        for (i = 0; i < 31; ++i) {
            input.lfFaceName[i] = faces[face][i];
            if (!faces[face][i]) break;
        }
        font = CreateFontIndirectA(&input);
        record(font != NULL); if (!font) ExitProcess(2);
        old = (HFONT)SelectObject(dc, font);
        record(old != NULL);
        for (count = 0; count <= 7; ++count) extent_a(dc, latin, count);
        for (count = 0; count <= 7; ++count) extent_a(dc, dbcs, count);
        for (count = 0; count <= 8; ++count) extent_w(dc, wide, count);
        extent_a(dc, latin, -1); extent_w(dc, wide, -1);
        extent_a(dc, NULL, 0); extent_w(dc, NULL, 0);
        record(GetObjectA(font, sizeof(output), &output));
        record(output.lfHeight); record(output.lfWeight); record(output.lfCharSet);
        record(SelectObject(dc, old) == font); record(DeleteObject(font));
    }
    /* CreateFontA exercises all 14 independently typed arguments. */
    font = CreateFontA(-19, 7, 0, 0, 500, 1, 0, 0, ANSI_CHARSET,
        OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, NONANTIALIASED_QUALITY,
        FIXED_PITCH, "Arial");
    record(font != NULL); old = (HFONT)SelectObject(dc, font);
    extent_a(dc, latin, 7); extent_w(dc, wide, 8);
    record(SelectObject(dc, old) == font); record(DeleteObject(font));
    record(SetTextColor(dc, RGB(32, 65, 129))); record(SetTextColor(dc, RGB(2, 4, 8)));
    record(SetBkColor(dc, RGB(19, 37, 73))); record(SetBkColor(dc, RGB(1, 3, 7)));
    record(SetBkMode(dc, TRANSPARENT)); record(SetBkMode(dc, OPAQUE)); record(SetBkMode(dc, 17));
    record(SetBkMode(dc, TRANSPARENT));
    {
        BITMAP bitmap_info;
        HBITMAP bitmap = CreateBitmap(0, 7, 1, 32, NULL);
        record(bitmap != NULL);
        record(GetObjectA(bitmap, sizeof(bitmap_info), &bitmap_info));
        record(bitmap_info.bmWidth); record(bitmap_info.bmHeight);
        record(bitmap_info.bmPlanes); record(bitmap_info.bmBitsPixel);
        record(DeleteObject(bitmap));
    }
#endif
    record(DeleteDC(dc));
    file = CreateFileA("gdi-text-query.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, words, used * 4, &written, NULL) || written != used * 4) ExitProcess(3);
    CloseHandle(file); ExitProcess(0);
}
