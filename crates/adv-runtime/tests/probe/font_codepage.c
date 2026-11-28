/* Build: i686-w64-mingw32-gcc -O2 -Wall font_codepage.c -o font_codepage.exe -lgdi32 */
/* Checks GdiGetCodePage and inline-hooked GetGlyphOutlineA across locales. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stdio.h>
#include <string.h>

typedef DWORD (WINAPI *gdi_codepage_fn)(HDC);
typedef DWORD (WINAPI *ggo_a_fn)(HDC, UINT, UINT, LPGLYPHMETRICS, DWORD, LPVOID, const MAT2 *);

static ggo_a_fn original_ggo_a;
static int hooked_calls;

static DWORD WINAPI hook_ggo_a(
    HDC dc, UINT ch, UINT format, LPGLYPHMETRICS metrics,
    DWORD size, LPVOID buffer, const MAT2 *matrix)
{
    hooked_calls++;
    return original_ggo_a(dc, ch, format, metrics, size, buffer, matrix);
}

static int install_ggo_a_hook(BYTE **trampoline_out, BYTE *saved, BYTE **target_out)
{
    HMODULE gdi = GetModuleHandleA("gdi32.dll");
    BYTE *target = gdi ? (BYTE *)GetProcAddress(gdi, "GetGlyphOutlineA") : NULL;
    BYTE *trampoline = VirtualAlloc(NULL, 16, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    DWORD old;
    if (!target || !trampoline || !VirtualProtect(target, 5, PAGE_EXECUTE_READWRITE, &old))
        return 0;
    memcpy(saved, target, 5);
    memcpy(trampoline, target, 5);
    trampoline[5] = 0xe9;
    *(DWORD *)(trampoline + 6) = (DWORD)(target + 5 - (trampoline + 10));
    original_ggo_a = (ggo_a_fn)trampoline;
    target[0] = 0xe9;
    *(DWORD *)(target + 1) = (DWORD)((BYTE *)hook_ggo_a - (target + 5));
    VirtualProtect(target, 5, old, &old);
    FlushInstructionCache(GetCurrentProcess(), target, 5);
    FlushInstructionCache(GetCurrentProcess(), trampoline, 16);
    *trampoline_out = trampoline;
    *target_out = target;
    return 1;
}

static void remove_ggo_a_hook(BYTE *trampoline, const BYTE *saved, BYTE *target)
{
    DWORD old;
    if (target && VirtualProtect(target, 5, PAGE_EXECUTE_READWRITE, &old)) {
        memcpy(target, saved, 5);
        VirtualProtect(target, 5, old, &old);
        FlushInstructionCache(GetCurrentProcess(), target, 5);
    }
    if (trampoline)
        VirtualFree(trampoline, 0, MEM_RELEASE);
}

int main(void)
{
    UINT acp = GetACP();
    HMODULE gdi = GetModuleHandleA("gdi32.dll");
    gdi_codepage_fn gdi_codepage =
        gdi ? (gdi_codepage_fn)GetProcAddress(gdi, "GdiGetCodePage") : NULL;
    HDC dc = CreateCompatibleDC(NULL);
    HFONT ansi_font, no_dfa_font, old;
    DWORD cp_ansi, cp_no_dfa;
    UINT expected_ansi = (acp == 936 || acp == 949 || acp == 950) ? acp : 1252;
    int ok = 1;

    if (!gdi_codepage || !dc)
        return 1;
    if (AddFontResourceExW(L"japanese.ttf", FR_PRIVATE, NULL) != 1)
        return 2;

    ansi_font = CreateFontW(-32, 0, 0, 0, 400, 0, 0, 0, ANSI_CHARSET,
                            OUT_TT_ONLY_PRECIS, CLIP_DEFAULT_PRECIS,
                            NONANTIALIASED_QUALITY, 0, L"ADV Japanese Fixture");
    no_dfa_font = CreateFontW(-32, 0, 0, 0, 400, 0, 0, 0, ANSI_CHARSET,
                              OUT_TT_ONLY_PRECIS, CLIP_DFA_DISABLE,
                              NONANTIALIASED_QUALITY, 0, L"ADV Japanese Fixture");
    if (!ansi_font || !no_dfa_font)
        return 3;

    old = SelectObject(dc, ansi_font);
    cp_ansi = gdi_codepage(dc);
    SelectObject(dc, no_dfa_font);
    cp_no_dfa = gdi_codepage(dc);

    printf("acp=%u ansi_cp=%lu no_dfa_cp=%lu\n", acp, cp_ansi, cp_no_dfa);
    ok &= cp_ansi == expected_ansi && cp_no_dfa == 1252;

    if (acp == 936 || acp == 950) {
        /* U+3042 (GBK 0xA4A2) has an authored outline; U+4E01 (Big5 0xA441)
         * differs from its CP1252 low byte 'A' (0x41). */
        UINT packed = (acp == 936) ? 0xa4a2 : 0xa441;
        UINT wide = (acp == 936) ? 0x3042 : 0x4e01;
        MAT2 identity = {{0, 1}, {0, 0}, {0, 0}, {0, 1}};
        GLYPHMETRICS gm_w = {0}, gm_a = {0}, gm_latin = {0};
        BYTE *buf_w = NULL, *buf_a = NULL;
        DWORD size_w, size_a, size_latin;
        BYTE *trampoline = NULL, *target = NULL, saved[5];

        SelectObject(dc, ansi_font);
        size_w = GetGlyphOutlineW(dc, wide, GGO_GRAY8_BITMAP, &gm_w, 0, NULL, &identity);
        size_latin = GetGlyphOutlineW(dc, packed & 0xff, GGO_GRAY8_BITMAP,
                                      &gm_latin, 0, NULL, &identity);
        if (size_w == GDI_ERROR)
            return 4;
        if (size_w > 0) {
            buf_w = HeapAlloc(GetProcessHeap(), 0, size_w);
            if (!buf_w || GetGlyphOutlineW(dc, wide, GGO_GRAY8_BITMAP, &gm_w,
                                           size_w, buf_w, &identity) != size_w)
                return 5;
        }

        if (!install_ggo_a_hook(&trampoline, saved, &target))
            return 6;
        size_a = GetGlyphOutlineA(dc, packed, GGO_GRAY8_BITMAP, &gm_a, 0, NULL, &identity);
        if (size_a > 0 && size_a != GDI_ERROR) {
            buf_a = HeapAlloc(GetProcessHeap(), 0, size_a);
            if (buf_a)
                size_a = GetGlyphOutlineA(dc, packed, GGO_GRAY8_BITMAP, &gm_a,
                                          size_a, buf_a, &identity);
        }
        remove_ggo_a_hook(trampoline, saved, target);

        {
            int expected_calls = size_w > 0 ? 2 : 1;
            int bytes_ok = size_w == 0 || (buf_a && !memcmp(buf_a, buf_w, size_w));
            int matched = hooked_calls == expected_calls
                && size_a == size_w
                && !memcmp(&gm_a, &gm_w, sizeof(gm_a))
                && bytes_ok
                && (size_latin != size_w || memcmp(&gm_latin, &gm_w, sizeof(gm_w)) != 0);
            printf("hooked_ggo=%d\n", matched);
            ok &= matched;
        }
        if (buf_a) HeapFree(GetProcessHeap(), 0, buf_a);
        if (buf_w) HeapFree(GetProcessHeap(), 0, buf_w);
    }

    SelectObject(dc, old);
    DeleteObject(ansi_font);
    DeleteObject(no_dfa_font);
    DeleteDC(dc);
    return ok ? 0 : 7;
}
