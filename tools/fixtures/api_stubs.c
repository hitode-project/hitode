#include "win32_min.h"

API LONG WINAPI DWriteCreateFactory(UINT, const void *, void **);
API LONG WINAPI D2D1CreateFactory(UINT, const void *, const void *, void **);
API DWORD WINAPI GetLastError(void);

/* Static imports must resolve even though neither optional renderer exists.
   Strict mode stops at the first call; discovery records both failure paths. */
void mainCRTStartup(void) {
    const DWORD iid[4] = {0, 0, 0, 0};
    void *factory = NULL;
    if ((DWORD)DWriteCreateFactory(0, iid, &factory) != 0x80004001u) ExitProcess(40);
    if (GetLastError() != 120 || factory != NULL) ExitProcess(41);
    if ((DWORD)D2D1CreateFactory(0, iid, NULL, &factory) != 0x80004001u) ExitProcess(42);
    if (GetLastError() != 120 || factory != NULL) ExitProcess(43);
    ExitProcess(0);
}
