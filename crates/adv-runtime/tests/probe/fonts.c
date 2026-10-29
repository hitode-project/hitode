/* Authored font discovery, aliases, TrueType enumeration of TrueType and CFF
 * faces, and glyph pixels.
 * Build: i686-w64-mingw32-gcc -O2 -Wall fonts.c -o fonts.exe -lgdi32 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static int check_override(HDC dc, int explicit)
{
    LOGFONTW request = {0}, saved, actual = {0};
    SIZE size = {0};
    HFONT font, old;
    int ok;
    request.lfHeight = -40;
    lstrcpyW(request.lfFaceName, L"aDv jApAnEsE fIxTuRe");
    saved = request;
    font = CreateFontIndirectW(&request);
    old = SelectObject(dc, font);
    ok = !memcmp(&request, &saved, sizeof(request));
    ok &= GetObjectW(font, sizeof(actual), &actual) == sizeof(actual);
    ok &= !lstrcmpW(actual.lfFaceName, explicit ? L"ADV Resource Fixture" : saved.lfFaceName);
    if (explicit) ok &= GetTextExtentPoint32W(dc, L"B", 1, &size) && size.cx == 20;
    SelectObject(dc, old);
    DeleteObject(font);
    return ok;
}

struct enumeration { int count, truetype; };
static int CALLBACK enumerate(const LOGFONTW *font, const TEXTMETRICW *metrics,
                              DWORD type, LPARAM param)
{
    struct enumeration *result = (void *)param;
    result->count++;
    result->truetype |= (type & TRUETYPE_FONTTYPE) &&
                       metrics->tmCharSet == SHIFTJIS_CHARSET;
    return 1;
}

static int check_face(HDC dc, const WCHAR *name)
{
    struct enumeration result = {0};
    LOGFONTW query = {0};
    GLYPHMETRICS glyph = {0};
    MAT2 transform = {{0, 1}, {0, 0}, {0, 0}, {0, 1}};
    DWORD length;
    HFONT font, old;
    query.lfCharSet = SHIFTJIS_CHARSET;
    lstrcpyW(query.lfFaceName, name);
    EnumFontFamiliesExW(dc, &query, enumerate, (LPARAM)&result, 0);
    font = CreateFontW(-32, 0, 0, 0, 400, 0, 0, 0, SHIFTJIS_CHARSET,
                       OUT_TT_ONLY_PRECIS, 0, NONANTIALIASED_QUALITY, 0, name);
    old = SelectObject(dc, font);
    length = GetGlyphOutlineW(dc, 0x3042, GGO_GRAY8_BITMAP, &glyph, 0, NULL, &transform);
    SelectObject(dc, old);
    DeleteObject(font);
    return result.count > 0 && result.truetype && length != GDI_ERROR && length > 0;
}

int main(int argc, char **argv)
{
    static const WCHAR text[] = {0x3042, 0};
    BITMAPINFO info = {{sizeof(BITMAPINFOHEADER), 64, -48, 1, 32, BI_RGB}};
    HDC dc = CreateCompatibleDC(NULL);
    if (argc > 1) {
        int ok = check_override(dc, !strcmp(argv[1], "explicit"));
        FILE *result = fopen("font-child-result", "wb");
        if (!result) return 6;
        fputc(ok, result);
        fclose(result);
        DeleteDC(dc);
        return ok ? 0 : 4;
    }
    FILE *choices = fopen("choices.txt", "r");
    int explicit = choices != NULL;
    if (choices) {
        BOOL smoothing = TRUE;
        SIZE chosen = {0};
        HFONT choice = CreateFontW(-40, 0, 0, 0, 400, 0, 0, 0, ANSI_CHARSET,
                                   0, 0, DEFAULT_QUALITY, 0, L"ADV Host Choice");
        HFONT previous = SelectObject(dc, choice);
        int selected = GetTextExtentPoint32W(dc, L"B", 1, &chosen) && chosen.cx == 20;
        selected &= SystemParametersInfoW(SPI_GETFONTSMOOTHING, 0, &smoothing, 0) && !smoothing;
        SelectObject(dc, previous); DeleteObject(choice); fclose(choices);
        printf("choices=%d\n", selected);
        if (!selected) return 2;
    } else {
        HKEY key;
        DWORD type, length = 0;
        BOOL smoothing = FALSE;
        int defaults = RegOpenKeyExW(HKEY_LOCAL_MACHINE, L"Software\\Microsoft\\Windows NT\\CurrentVersion\\FontSubstitutes", 0, KEY_READ, &key) == ERROR_SUCCESS;
        if (defaults) {
            defaults &= RegQueryValueExW(key, L"ADV Host Choice", NULL, &type, NULL, &length) == ERROR_FILE_NOT_FOUND;
            RegCloseKey(key);
        }
        defaults &= SystemParametersInfoW(SPI_GETFONTSMOOTHING, 0, &smoothing, 0) && smoothing;
        printf("defaults=%d\n", defaults);
        if (!defaults) return 3;
    }
    {
        STARTUPINFOA startup = { sizeof(startup) };
        PROCESS_INFORMATION process;
        char command[64];
        int override = check_override(dc, explicit), child = 0;
        DeleteFileA("font-child-result");
        sprintf(command, "fonts.exe %s", explicit ? "explicit" : "default");
        if (CreateProcessA(NULL, command, NULL, NULL, FALSE, 0, NULL, NULL, &startup, &process)) {
            child = 1;
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
        }
        printf("override=%d child=%d\n", override, child);
        if (!override || !child) return 5;
    }
    DWORD *pixels;
    HBITMAP bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, (void **)&pixels, NULL, 0);
    HFONT font, old;
    SIZE size = {0};
    int i, ink = 0, ok = 1;
    const WCHAR *names[] = {
        L"MS Gothic", L"\xff2d\xff33 \x30b4\x30b7\x30c3\x30af",
        L"MS PGothic", L"MS UI Gothic", L"MS \x30b4\x30b7\x30c3\x30af"
    };
    for (i = 0; i < sizeof(names) / sizeof(names[0]); ++i) ok &= check_face(dc, names[i]);
    printf("aliases=%d\n", ok);
    /* A CFF face enumerates and rasterizes as TrueType, like the standard
       families games filter for. */
    i = check_face(dc, L"ADV Japanese CFF Fixture");
    printf("cff=%d\n", i);
    ok &= i;
    SelectObject(dc, bitmap);
    ZeroMemory(pixels, 64 * 48 * 4);
    font = CreateFontW(-32, 0, 0, 0, 400, 0, 0, 0, SHIFTJIS_CHARSET,
                       0, 0, NONANTIALIASED_QUALITY, 0, L"MS Gothic");
    old = SelectObject(dc, font);
    SetTextColor(dc, RGB(255, 255, 255));
    SetBkMode(dc, TRANSPARENT);
    ok &= TextOutW(dc, 0, 0, text, 1);
    GdiFlush();
    for (i = 0; i < 64 * 48; ++i) ink += pixels[i] == 0xffffff;
    printf("pixels=%d\n", ink > 0);
    ok &= ink > 0;
    SelectObject(dc, old);
    DeleteObject(font);
    /* A font explicitly installed by the guest keeps its own family/metrics. */
    ok &= AddFontResourceExW(L"private.ttf", FR_PRIVATE, NULL) == 1;
    font = CreateFontW(-40, 0, 0, 0, 400, 0, 0, 0, ANSI_CHARSET,
                       0, 0, NONANTIALIASED_QUALITY, 0, L"ADV Resource Fixture");
    old = SelectObject(dc, font);
    ok &= GetTextExtentPoint32W(dc, L"B", 1, &size) && size.cx == 20;
    printf("private=%d\n", size.cx == 20);
    SelectObject(dc, old);
    DeleteObject(font);
    {
        /* ANSI window text converts through win32u's code page tables. */
        WNDCLASSA wc = {0};
        char title[8] = {0};
        HWND window;
        wc.lpfnWndProc = DefWindowProcA;
        wc.hInstance = GetModuleHandleA(NULL);
        wc.lpszClassName = "adv-ansi";
        RegisterClassA(&wc);
        window = CreateWindowExA(0, "adv-ansi", "\x82\xa0", 0, 0, 0, 1, 1,
                                 NULL, NULL, wc.hInstance, NULL);
        i = window && GetWindowTextLengthA(window) == 2
            && GetWindowTextA(window, title, sizeof(title)) == 2
            && !strcmp(title, "\x82\xa0");
        printf("ansi=%d\n", i);
        ok &= i;
        DestroyWindow(window);
    }
    DeleteDC(dc);
    DeleteObject(bitmap);
    return ok ? 0 : 1;
}
