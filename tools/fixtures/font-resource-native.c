/* Authored no-window font-resource contract oracle. Only the repo font fixture. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

static DWORD values[128], used;
static void record(DWORD value) { values[used++] = value; }
static int CALLBACK count_font(const LOGFONTW *font, const TEXTMETRICW *metric,
                              DWORD kind, LPARAM data) {
    (void)font; (void)metric; (void)kind;
    ++*(DWORD *)data;
    return 1;
}
static DWORD enumerate(HDC dc) {
    LOGFONTW query = {0};
    static const WCHAR face[] = L"ADV Resource Fixture";
    DWORD count = 0, i;
    query.lfCharSet = DEFAULT_CHARSET;
    for (i = 0; i < sizeof(face) / sizeof(face[0]); ++i) query.lfFaceName[i] = face[i];
    EnumFontFamiliesExW(dc, &query, count_font, (LPARAM)&count, 0);
    return count;
}
static void add(DWORD flags, void *reserved) {
    SetLastError(0x12345678);
    record(AddFontResourceExW(L"font-resource.ttf", flags, reserved));
    record(GetLastError());
}
static void remove_font(DWORD flags, void *reserved) {
    SetLastError(0x12345678);
    record(RemoveFontResourceExW(L"font-resource.ttf", flags, reserved));
    record(GetLastError());
}
void mainCRTStartup(void) {
    HDC dc = CreateCompatibleDC(NULL);
    HANDLE output;
    DWORD written;
    if (!dc) ExitProcess(1);
    record(enumerate(dc));
    add(FR_PRIVATE, NULL); record(enumerate(dc));
    remove_font(0, NULL); record(enumerate(dc));
    remove_font(FR_PRIVATE | FR_NOT_ENUM, NULL); record(enumerate(dc));
    add(FR_PRIVATE, NULL);
    remove_font(FR_PRIVATE, NULL); record(enumerate(dc));
    remove_font(FR_PRIVATE, NULL); record(enumerate(dc));
    add(FR_PRIVATE | FR_NOT_ENUM, NULL); record(enumerate(dc));
    remove_font(FR_PRIVATE, NULL);
    remove_font(FR_PRIVATE | FR_NOT_ENUM, NULL); record(enumerate(dc));
    add(FR_PRIVATE | 0x40, NULL); record(enumerate(dc));
    remove_font(FR_PRIVATE, NULL);
    remove_font(FR_PRIVATE | 0x40, NULL); record(enumerate(dc));
    add(FR_PRIVATE, (void *)1);
    remove_font(FR_PRIVATE, (void *)1);
    remove_font(FR_PRIVATE, NULL);
    DeleteDC(dc);
    output = CreateFileA("font-resource-native.bin", GENERIC_WRITE, 0, NULL,
                         CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (output == INVALID_HANDLE_VALUE || !WriteFile(output, values, used * 4, &written, NULL)
        || written != used * 4) ExitProcess(2);
    CloseHandle(output);
    ExitProcess(0);
}
