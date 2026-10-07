/* Authored USER32/GDI oracle: invisible windows, no game or host cursor changes. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

static DWORD report[280], used;
static HDC last_dc, paint_dc;
static BOOL capture_paint;

static void emit(DWORD value) {
    if (used >= sizeof(report) / sizeof(report[0])) ExitProcess(90);
    report[used++] = value;
}

static void state(HDC dc) {
    HGDIOBJ font = GetCurrentObject(dc, OBJ_FONT);
    emit(GetStretchBltMode(dc));
    emit(GetBkMode(dc));
    emit(GetBkColor(dc));
    emit(GetTextColor(dc));
    emit(font == GetStockObject(SYSTEM_FONT));
    emit(font == GetStockObject(ANSI_FIXED_FONT));
    emit(font == GetStockObject(ANSI_VAR_FONT));
}

static LRESULT WINAPI procedure(HWND window, UINT message, WPARAM wp, LPARAM lp) {
    if (message == WM_PAINT && capture_paint) {
        PAINTSTRUCT paint;
        paint_dc = BeginPaint(window, &paint);
        emit(paint_dc != NULL);
        emit(paint_dc == last_dc);
        state(paint_dc);
        emit(EndPaint(window, &paint));
        return 0;
    }
    return DefWindowProcA(window, message, wp, lp);
}

static void probe(HINSTANCE instance, UINT style, const char *name) {
    static WNDCLASSA definition;
    HWND window;
    HDC first, second, final;
    definition.style = style;
    definition.lpfnWndProc = procedure;
    definition.hInstance = instance;
    definition.lpszClassName = name;
    emit(style);
    emit(RegisterClassA(&definition) != 0);
    window = CreateWindowExA(0, name, "native DC lifecycle", WS_POPUP,
                            0, 0, 32, 32, NULL, NULL, instance, NULL);
    emit(window != NULL);
    if (!window) ExitProcess(2);
    first = GetDC(window);
    emit(first != NULL);
    state(first);
    emit(SetStretchBltMode(first, COLORONCOLOR));
    emit(SetBkMode(first, TRANSPARENT));
    emit(SetBkColor(first, RGB(0x11, 0x22, 0x33)));
    emit(SetTextColor(first, RGB(0x44, 0x55, 0x66)));
    emit(SelectObject(first, GetStockObject(ANSI_FIXED_FONT)) != NULL);
    emit(SaveDC(first));
    SetStretchBltMode(first, HALFTONE);
    SetBkMode(first, OPAQUE);
    SetBkColor(first, RGB(4, 5, 6));
    SetTextColor(first, RGB(1, 2, 3));
    SelectObject(first, GetStockObject(ANSI_VAR_FONT));
    emit(ReleaseDC(window, first));
    second = GetDC(window);
    emit(second != NULL);
    emit(second == first);
    state(second);
    SetLastError(0x12345678);
    emit(RestoreDC(second, -1));
    emit(GetLastError());
    state(second);
    emit(ReleaseDC(window, second));
    last_dc = second;
    paint_dc = NULL;
    capture_paint = TRUE;
    emit(InvalidateRect(window, NULL, FALSE));
    SendMessageA(window, WM_PAINT, 0, 0);
    capture_paint = FALSE;
    final = GetDC(window);
    emit(final != NULL);
    emit(final == paint_dc);
    state(final);
    /* The common DC has been returned to USER's cache. The private DC remains
       attached to its window until DestroyWindow, regardless of ReleaseDC. */
    emit(ReleaseDC(window, final));
    SetLastError(0x12345678);
    emit(GetObjectType(final));
    emit(GetLastError());
    emit(DestroyWindow(window));
    SetLastError(0x12345678);
    emit(GetObjectType(final));
    emit(GetLastError());
    SetLastError(0x12345678);
    emit(GetStretchBltMode(final));
    emit(GetLastError());
    SetLastError(0x12345678);
    emit(ReleaseDC(window, final));
    emit(GetLastError());
    SetLastError(0x12345678);
    emit(GetDC(window) != NULL);
    emit(GetLastError());
    emit(UnregisterClassA(name, instance));
}

static void independent_windows(HINSTANCE instance) {
    static WNDCLASSA definition;
    const char *name = "AuthoredSeparatePrivateDCs";
    HWND first, second;
    HDC first_dc, second_dc, window_dc, repeated;
    definition.style = CS_OWNDC;
    definition.lpfnWndProc = procedure;
    definition.hInstance = instance;
    definition.lpszClassName = name;
    emit(0x44434c32); /* DCL2 */
    emit(RegisterClassA(&definition) != 0);
    first = CreateWindowExA(0, name, "first", WS_POPUP, 0, 0, 32, 32,
                           NULL, NULL, instance, NULL);
    second = CreateWindowExA(0, name, "second", WS_POPUP, 0, 0, 32, 32,
                            NULL, NULL, instance, NULL);
    emit(first != NULL); emit(second != NULL);
    if (!first || !second) ExitProcess(5);
    first_dc = GetDC(first); second_dc = GetDC(second);
    emit(first_dc != NULL); emit(second_dc != NULL);
    emit(first_dc != second_dc);
    SetStretchBltMode(first_dc, COLORONCOLOR);
    SetStretchBltMode(second_dc, HALFTONE);
    SetTextColor(first_dc, RGB(1, 2, 3));
    SetTextColor(second_dc, RGB(4, 5, 6));
    SelectObject(first_dc, GetStockObject(ANSI_FIXED_FONT));
    SelectObject(second_dc, GetStockObject(ANSI_VAR_FONT));
    emit(ReleaseDC(first, first_dc)); emit(ReleaseDC(second, second_dc));
    emit(GetDC(first) == first_dc); emit(GetDC(second) == second_dc);
    state(first_dc); state(second_dc);
    /* GetWindowDC always acquires a common nonclient DC, even for CS_OWNDC.
       Its default attributes must not overwrite the persistent client DC. */
    window_dc = GetWindowDC(first);
    emit(window_dc != NULL); emit(window_dc == first_dc);
    state(window_dc);
    emit(SetStretchBltMode(window_dc, WHITEONBLACK));
    emit(ReleaseDC(first, window_dc));
    repeated = GetWindowDC(first);
    emit(repeated != NULL); emit(repeated == window_dc);
    state(repeated);
    emit(ReleaseDC(first, repeated));
    state(first_dc); state(second_dc);
    emit(DestroyWindow(first));
    SetLastError(0x12345678);
    emit(GetStretchBltMode(first_dc)); emit(GetLastError());
    SetLastError(0x12345678);
    emit(ReleaseDC(first, first_dc)); emit(GetLastError());
    SetLastError(0x12345678);
    emit(GetDC(first) != NULL); emit(GetLastError());
    state(second_dc);
    emit(ReleaseDC(second, second_dc));
    emit(DestroyWindow(second));
    SetLastError(0x12345678);
    emit(GetStretchBltMode(second_dc)); emit(GetLastError());
    emit(UnregisterClassA(name, instance));
}

void mainCRTStartup(void) {
    HANDLE file;
    DWORD written;
    HINSTANCE instance = GetModuleHandleA(NULL);
    emit(0x44434c31); /* DCL1 */
    probe(instance, 0, "AuthoredCommonDC");
    probe(instance, CS_OWNDC, "AuthoredPrivateDC");
    independent_windows(instance);
    file = CreateFileA("window-dc-native.bin", GENERIC_WRITE, 0, NULL,
                       CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(3);
    if (!WriteFile(file, report, used * sizeof(DWORD), &written, NULL) ||
        written != used * sizeof(DWORD)) ExitProcess(4);
    CloseHandle(file);
    ExitProcess(0);
}
