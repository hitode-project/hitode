/* Build: i686-w64-mingw32-gcc -O2 -Wall nt_registry.c -o nt_registry.exe -ladvapi32
 * Authored repeated registry reads and mutation across a process family. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static const char path[] = "Software\\HitodeRegistryCache";
static void check(const char *name, BOOL ok)
{
    printf("%s=%s\n", name, ok ? "ok" : "FAIL");
    fflush(stdout);
    if (!ok) ExitProcess(1);
}
static BOOL query(HKEY key, DWORD expected)
{
    DWORD value = 0, type = 0, size = sizeof(value);
    return !RegQueryValueExA(key, "value", NULL, &type, (BYTE *)&value, &size)
        && type == REG_DWORD && size == sizeof(value) && value == expected;
}
int main(void)
{
    HKEY key;
    DWORD value = 42, size, type, code;
    char exe[MAX_PATH], command[MAX_PATH + 32];
    STARTUPINFOA startup = {sizeof(startup)};
    PROCESS_INFORMATION child;
    if (RegCreateKeyExA(HKEY_CURRENT_USER, path, 0, NULL, 0, KEY_ALL_ACCESS, NULL, &key, NULL)) return 1;
    if (strstr(GetCommandLineA(), "--child")) {
        value = 99;
        return RegSetValueExA(key, "value", 0, REG_DWORD, (BYTE *)&value, sizeof(value));
    }
    if (RegSetValueExA(key, "value", 0, REG_DWORD, (BYTE *)&value, sizeof(value))) return 1;
    for (int i = 0; i < 1000; ++i) if (!query(key, 42)) return 1;
    check("repeat", TRUE);
    size = 0; type = 0;
    check("size", !RegQueryValueExA(key, "value", NULL, &type, NULL, &size)
        && size == 4 && type == REG_DWORD);
    size = 1;
    check("short", RegQueryValueExA(key, "value", NULL, &type, (BYTE *)&value, &size)
        == ERROR_MORE_DATA && size == 4);
    value = 43;
    check("update", !RegSetValueExA(key, "value", 0, REG_DWORD, (BYTE *)&value, sizeof(value)) && query(key, 43));
    GetModuleFileNameA(NULL, exe, sizeof(exe));
    snprintf(command, sizeof(command), "\"%s\" --child", exe);
    if (!CreateProcessA(NULL, command, NULL, NULL, FALSE, 0, NULL, NULL, &startup, &child)) return 1;
    check("child_update", WaitForSingleObject(child.hProcess, 30000) == WAIT_OBJECT_0
        && GetExitCodeProcess(child.hProcess, &code) && code == 0 && query(key, 99));
    CloseHandle(child.hThread); CloseHandle(child.hProcess);
    size = sizeof(value);
    check("delete", !RegDeleteValueA(key, "value")
        && RegQueryValueExA(key, "value", NULL, NULL, (BYTE *)&value, &size) == ERROR_FILE_NOT_FOUND);
    RegCloseKey(key);
    if (RegOpenKeyExA(HKEY_CURRENT_USER, path, 0, KEY_ALL_ACCESS, &key)) return 1;
    value = 44;
    check("reopen", !RegSetValueExA(key, "value", 0, REG_DWORD, (BYTE *)&value, sizeof(value)) && query(key, 44));
    RegCloseKey(key);
    RegDeleteKeyA(HKEY_CURRENT_USER, path);
    return 0;
}
