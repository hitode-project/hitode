/* Authored timed waits, external synchronization and process-family clocks.
 * Build: i686-w64-mingw32-gcc -O2 -Wall time_skip.c -o time_skip.exe */
#include <windows.h>
#include <stdio.h>
#include <string.h>
int main(void)
{
    if (strstr(GetCommandLineA(), "--child")) { Sleep(250); return 0; }
    Sleep(2000);
    SleepEx(2000, TRUE);
    HANDLE event = CreateEventW(NULL, TRUE, FALSE, NULL);
    if (WaitForSingleObject(event, 250) != WAIT_TIMEOUT) return 1;
    CloseHandle(event);
    char exe[MAX_PATH], command[MAX_PATH + 32];
    STARTUPINFOA startup = {sizeof(startup)};
    PROCESS_INFORMATION child;
    GetModuleFileNameA(NULL, exe, sizeof(exe));
    snprintf(command, sizeof(command), "\"%s\" --child", exe);
    if (!CreateProcessA(NULL, command, NULL, NULL, FALSE, 0, NULL, NULL, &startup, &child)) return 2;
    if (WaitForSingleObject(child.hProcess, 5000) != WAIT_OBJECT_0) return 3;
    CloseHandle(child.hThread); CloseHandle(child.hProcess);
    Sleep(250); /* A retired child must not re-enable acceleration. */
    puts("waits=ok");
    return 0;
}
