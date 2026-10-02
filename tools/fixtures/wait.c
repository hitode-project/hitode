#include "win32_min.h"

/* An unsignalled event parks the process forever. The frontend's wall-clock
   limit must wake independently of guest deadlines, including in headless mode. */
void mainCRTStartup(void) {
    HANDLE event = CreateEventA(NULL, 0, 0, NULL);
    if (!event) ExitProcess(30);
    WaitForSingleObject(event, INFINITE);
    ExitProcess(31);
}
