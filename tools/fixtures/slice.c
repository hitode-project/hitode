/* Finite pure computation: each volatile store must execute exactly once. */
#include "win32_min.h"
#define ITERATIONS 2000000u
static volatile DWORD counts[2];
static DWORD results[2];
static DWORD WINAPI worker(void *argument) {
    DWORD index = (DWORD)argument, i, result = index + 7;
    for (i = 0; i < ITERATIONS; ++i) { ++counts[index]; result += 3; }
    results[index] = result;
    return 0;
}
void mainCRTStartup(void) {
    HANDLE first = CreateThread(NULL, 0, worker, (void *)0, 0, NULL);
    HANDLE second = CreateThread(NULL, 0, worker, (void *)1, 0, NULL);
    if (!first || !second) ExitProcess(73);
    if (WaitForSingleObject(first, INFINITE) || WaitForSingleObject(second, INFINITE)) ExitProcess(74);
    if (counts[0] != ITERATIONS || counts[1] != ITERATIONS) ExitProcess(75);
    if (results[0] != ITERATIONS * 3 + 7 || results[1] != ITERATIONS * 3 + 8) ExitProcess(76);
    CloseHandle(first); CloseHandle(second); ExitProcess(0);
}
