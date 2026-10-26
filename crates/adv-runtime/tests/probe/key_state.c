/* Headless queue keyboard state probe.
 * Build: i686-w64-mingw32-gcc -O1 -o key_state.exe key_state.c -luser32 */
#include <windows.h>
#include <stdio.h>

int main(void)
{
    BYTE keys[256] = {0};
    GetQueueStatus(QS_ALLINPUT);
    printf("keys=");
    for (unsigned key = 0; key < 256; ++key)
        printf("%04x", (unsigned short)GetKeyState(key));
    keys[0] = 0x01;
    keys[1] = 0x80;
    keys['A'] = 0x81;
    keys['Z'] = 0x7f;
    keys[VK_LSHIFT] = 0xff;
    keys[VK_RSHIFT] = 0x40;
    keys[254] = 0xc0;
    keys[255] = 0x80;
    if (!SetKeyboardState(keys)) return 1;
    for (unsigned key = 0; key < 256; ++key)
        printf("%04x", (unsigned short)GetKeyState(key));
    printf("\n");
    return 0;
}
