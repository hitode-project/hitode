/* Build: i686-w64-mingw32-gcc -Os -nostdlib -Wl,--entry,_mainCRTStartup -o vorbisfile.exe vorbisfile.c -lkernel32 */
#include <windows.h>

typedef int (__cdecl *CLEAR)(void *);
typedef int (__cdecl *ATTACHED)(void);
void mainCRTStartup(void) {
    HMODULE dll = LoadLibraryA("vorbisfile.dll");
    CLEAR named = (CLEAR)GetProcAddress(dll, "ov_clear");
    CLEAR ordinal = (CLEAR)GetProcAddress(dll, (const char *)3);
    ATTACHED attached = (ATTACHED)GetProcAddress(dll, "fixture_attached");
    if (!dll || !named || named != ordinal || !attached || attached() != 1) ExitProcess(1);
    BYTE *code = (BYTE *)dll + 0x1000;
    if (code[0] != 0xb8 || code[1] != 77 || code[5] != 0xc3) ExitProcess(5);
    /* The authored guest DLL returns 77; the host clears an unopened vf. */
    int result = named(NULL);
    if (result != 77 && result != 0) ExitProcess(2);
    if (!FreeLibrary(dll)) ExitProcess(3);
    dll = LoadLibraryA("vorbisfile.dll");
    named = (CLEAR)GetProcAddress(dll, "ov_clear");
    attached = (ATTACHED)GetProcAddress(dll, "fixture_attached");
    if (!dll || !named || named(NULL) != result || !attached || attached() != 1) ExitProcess(4);
    ExitProcess(result == 77 ? 77 : 0);
}
