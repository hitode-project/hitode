/* Authored sequential-session ownership check.
 * Build: i686-w64-mingw32-gcc -O2 -Wall session_lifecycle.c -o session_lifecycle.exe -ladvapi32 */
#include <windows.h>
#include <string.h>

int main(void)
{
    HKEY key;
    DWORD runs = 0, size = sizeof(runs), written, observed[2];
    HANDLE event, ready;
    if (strstr(GetCommandLineA(), " child")) {
        event = CreateEventA(NULL, TRUE, FALSE, "HitodeChildLifetime");
        if (!event || GetLastError() == ERROR_ALREADY_EXISTS) return 6;
        ready = CreateFileA("child-ready", GENERIC_WRITE, FILE_SHARE_READ, NULL,
                            CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
        if (ready == INVALID_HANDLE_VALUE) return 7;
        CloseHandle(ready);
        for (;;) Sleep(10);
    }
    if (RegCreateKeyExA(HKEY_CURRENT_USER, "Software\\HitodeLifecycle", 0, NULL,
                        0, KEY_ALL_ACCESS, NULL, &key, NULL)) return 1;
    RegQueryValueExA(key, "Runs", NULL, NULL, (BYTE *)&runs, &size);
    ++runs;
    if (RegSetValueExA(key, "Runs", 0, REG_DWORD, (BYTE *)&runs, sizeof(runs))) return 2;
    RegCloseKey(key);
    observed[0] = runs;
    observed[1] = 0;
    if (!RegOpenKeyExA(HKEY_LOCAL_MACHINE,
            "Software\\Microsoft\\Windows NT\\CurrentVersion\\FontSubstitutes", 0, KEY_READ, &key)) {
        char alias[64];
        size = sizeof(alias);
        observed[1] = !RegQueryValueExA(key, "HitodeLifetimeAlias", NULL, NULL, (BYTE *)alias, &size);
        RegCloseKey(key);
    }
    event = CreateEventA(NULL, TRUE, FALSE, "HitodeSessionLifetime");
    if (!event || GetLastError() == ERROR_ALREADY_EXISTS) return 3;
    event = CreateEventA(NULL, TRUE, FALSE, "HitodeChildLifetime");
    if (!event || GetLastError() == ERROR_ALREADY_EXISTS) return 8;
    CloseHandle(event);
    if (GetFileAttributesA("spawn") != INVALID_FILE_ATTRIBUTES) {
        STARTUPINFOA startup = { sizeof(startup) };
        PROCESS_INFORMATION process;
        char command[] = "session_lifecycle.exe child";
        if (!CreateProcessA(NULL, command, NULL, NULL, FALSE, 0, NULL, NULL,
                            &startup, &process)) return 9;
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
        while (GetFileAttributesA("child-ready") == INVALID_FILE_ATTRIBUTES) Sleep(10);
    }
    ready = CreateFileA("ready", GENERIC_WRITE, FILE_SHARE_READ, NULL,
                        CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (ready == INVALID_HANDLE_VALUE) return 4;
    if (!WriteFile(ready, observed, sizeof(observed), &written, NULL) || written != sizeof(observed)) return 5;
    CloseHandle(ready);
    if (GetFileAttributesA("hold") != INVALID_FILE_ATTRIBUTES)
        for (;;) Sleep(10);
    /* Leave the handle open: session teardown must close even an abandoned one. */
    return 0;
}
