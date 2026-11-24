/* A setup that asks for its next disc: it keeps a file of the first disc
 * open, waits until drive D: holds a disc with another label, then reads the
 * new disc and the file it kept.
 * Build: i686-w64-mingw32-gcc -O2 -Wall discswap.c -o discswap.exe */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static int read_text(HANDLE file, char *text)
{
    DWORD count;
    SetFilePointer(file, 0, NULL, FILE_BEGIN);
    if (!ReadFile(file, text, 15, &count, NULL))
        return 0;
    text[count] = 0;
    return 1;
}

static HANDLE open_text(void)
{
    return CreateFileA("D:\\DISC.TXT", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL); /* privacy: allow authored guest path */
}

static void label(char *out)
{
    out[0] = 0;
    GetVolumeInformationA("D:\\", out, 32, NULL, NULL, NULL, NULL, 0); /* privacy: allow authored guest path */
}

int main(void)
{
    char first[32], now[32], text[16];
    label(first);
    HANDLE kept = open_text();
    if (kept == INVALID_HANDLE_VALUE || !read_text(kept, text) || strcmp(text, "first")) {
        printf("first=FAIL\n");
        return 1;
    }
    printf("first=%s label=%s\ninsert=2\n", text, first);
    fflush(stdout);
    for (int i = 0; i < 400; i++) {
        label(now);
        if (strcmp(now, first))
            break;
        Sleep(50);
    }
    printf("label=%s\n", now);
    HANDLE next = open_text();
    int second = strcmp(now, first) && next != INVALID_HANDLE_VALUE && read_text(next, text) &&
                 !strcmp(text, "second");
    printf("second=%s\n", second ? "ok" : "FAIL");
    int still = read_text(kept, text) && !strcmp(text, "first");
    printf("kept=%s\n", still ? "ok" : "FAIL");
    fflush(stdout);
    return second && still ? 0 : 1;
}
