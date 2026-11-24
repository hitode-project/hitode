/* Blocks shrink in place, as on Windows 7, once the low-fragmentation heap
 * serves their size class.
 * Build: i686-w64-mingw32-gcc -O2 -Wall -o heap_shrink.exe heap_shrink.c */
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned failures;
static void check(const char *name, int passed)
{
    printf("%s=%s\n", name, passed ? "ok" : "FAIL");
    fflush(stdout);
    if (!passed) failures++;
}

/* Fills `count` blocks of `size` so the heap enables its bin for them. */
static int shrinks(HANDLE heap, SIZE_T size, SIZE_T smaller)
{
    static void *blocks[512];
    int same = 1;
    for (int i = 0; i < 512; i++) {
        blocks[i] = HeapAlloc(heap, 0, size);
        memset(blocks[i], i, size);
    }
    for (int i = 0; i < 512; i += 2) {
        void *shrunk = HeapReAlloc(heap, 0, blocks[i], smaller);
        same &= shrunk == blocks[i] && HeapSize(heap, 0, shrunk) == smaller &&
                ((unsigned char *)shrunk)[smaller - 1] == (unsigned char)i;
    }
    for (int i = 0; i < 512; i++) HeapFree(heap, 0, blocks[i]);
    return same;
}

int main(void)
{
    HANDLE heap = HeapCreate(0, 0, 0);
    check("lfh_shrink", shrinks(heap, 0x4000, 0x100));
    ULONG mode = 0;
    check("lfh_enabled", HeapQueryInformation(heap, HeapCompatibilityInformation, &mode, sizeof(mode), NULL) && mode == 2);
    check("lfh_shrink_small", shrinks(heap, 0x200, 0x10));
    void *grown = HeapAlloc(heap, 0, 0x100);
    check("grow", HeapReAlloc(heap, 0, grown, 0x6000) != NULL);
    HeapDestroy(heap);
    /* The C runtime's realloc keeps a shrunk buffer where it was. */
    void *buffers[512];
    int same = 1;
    for (int i = 0; i < 512; i++) buffers[i] = malloc(0x4000);
    for (int i = 0; i < 512; i++) same &= realloc(buffers[i], 0x40) == buffers[i];
    for (int i = 0; i < 512; i++) free(buffers[i]);
    check("crt_shrink", same);
    return failures ? 1 : 0;
}
