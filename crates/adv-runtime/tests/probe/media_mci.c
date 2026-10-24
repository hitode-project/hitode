/* i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 media_mci.c -o media_mci.exe -lwinmm */
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>
int main(void)
{
    const char *commands[] = {"open synthetic.mpg type mpegvideo alias clip", "play clip wait", "close clip"};
    unsigned i;
    for (i = 0; i < 3; i++) {
        MCIERROR error = mciSendStringA(commands[i], NULL, 0, NULL);
        if (error) { printf("FAIL mci %u %lu\n", i, error); return 1; }
    }
    puts("media mci ok"); return 0;
}
