#include "win32_min.h"

API DWORD WINAPI GetLastError(void);
API int WINAPI MessageBoxA(HWND, const char *, const char *, UINT);

/* An unknown feature probe is not a known missing SDK implementation. Its
   error message must still survive a headless run without a trace or logger. */
void mainCRTStartup(void) {
    HMODULE user = LoadLibraryA("user32.dll");
    if (!user) ExitProcess(70);
    if (GetProcAddress(user, "__adv_fixture_optional_export")) ExitProcess(71);
    DWORD error = GetLastError();
    int answer = MessageBoxA(0,
        "Cannot find function __adv_fixture_optional_export in USER32.dll\n"
        "Keep \"quotes\", tabs\tand a backslash \\ in the report.",
        "Diagnostic \"caption\"\nsecond line", 0x10);
    if (answer != 1 || GetLastError() != error) ExitProcess(72);
    ExitProcess(1);
}
