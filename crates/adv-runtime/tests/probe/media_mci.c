/* i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 media_mci.c -o media_mci.exe -lwinmm */
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>
#include <string.h>
int main(void)
{
    const char *commands[] = {"open synthetic.mpg type mpegvideo alias clip", "play clip wait", "close clip"};
    unsigned i;
    for (i = 0; i < 3; i++) {
        MCIERROR error = mciSendStringA(commands[i], NULL, 0, NULL);
        if (error) { printf("FAIL mci %u %lu\n", i, error); return 1; }
        if (i == 1) {
            char mode[32];
            DWORD start = GetTickCount();
            do {
                error = mciSendStringA("status clip mode", mode, sizeof(mode), NULL);
                if (error || GetTickCount() - start > 10000) return 2;
                if (!strcmp(mode, "stopped")) break;
                Sleep(10);
            } while (1);
        }
    }
    puts("media mci ok"); return 0;
}
