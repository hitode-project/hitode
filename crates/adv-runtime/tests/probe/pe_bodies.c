/* Authored code page boundary checks; no display or audio.
 * Build: i686-w64-mingw32-gcc -O2 -Wall pe_bodies.c -o pe_bodies.exe */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static DWORD hash = 2166136261u;
static void record(const void *ptr, unsigned len)
{
    const unsigned char *p = ptr;
    while (len--) hash = (hash ^ *p++) * 16777619u;
}
int main(void)
{
    const char sjis[] = {0x82, 0xa0, 0x82, 0xa2, 0x81, 0x40, 0, 0x81};
    const WCHAR unicode[] = {0x4e2d, 0x6587, 0x41, 0x20ac, 0xd800, 0, 0x4e00};
    WCHAR wide[32];
    char narrow[32];
    BOOL used;
    DWORD error;
    int i, ret;
    for (i = 0; i < 256; ++i)
    {
        memset(wide, 0xcc, sizeof(wide));
        SetLastError(0x12345678);
        ret = MultiByteToWideChar(932, (i & 1) ? MB_ERR_INVALID_CHARS : 0,
                                 sjis, (i & 2) ? sizeof(sjis) : -1,
                                 wide, (i & 4) ? 2 : 32);
        error = GetLastError();
        record(&ret, sizeof(ret)); record(&error, sizeof(error)); record(wide, sizeof(wide));
        memset(narrow, 0xcc, sizeof(narrow));
        used = 0xcccccccc;
        SetLastError(0x12345678);
        ret = WideCharToMultiByte(936, (i & 1) ? WC_NO_BEST_FIT_CHARS : 0,
                                 unicode, (i & 2) ? 7 : -1,
                                 narrow, (i & 4) ? 3 : 32, "!", &used);
        error = GetLastError();
        record(&ret, sizeof(ret)); record(&error, sizeof(error)); record(&used, sizeof(used)); record(narrow, sizeof(narrow));
    }
    printf("conversion=%08lx\n", (unsigned long)hash);
    return 0;
}
