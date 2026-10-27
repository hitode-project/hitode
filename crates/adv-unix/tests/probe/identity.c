/* The identity probe: prints what a program learns about its user and
 * machine from the environment, the system and the registry, one
 * `name=value` line each, and exits with 0.
 * Build: i686-w64-mingw32-gcc -O1 -mcrtdll=msvcrt-os -o identity.exe identity.c -ladvapi32 */
#include <windows.h>
#include <stdio.h>

static void variable(const char *name)
{
    char value[MAX_PATH];
    if (!GetEnvironmentVariableA(name, value, sizeof(value))) strcpy(value, "(none)");
    printf("%s=%s\n", name, value);
}

static void value(const char *label, const char *key, const char *name)
{
    char data[256];
    DWORD size = sizeof(data);
    if (RegGetValueA(HKEY_LOCAL_MACHINE, key, name, RRF_RT_REG_SZ, NULL, data, &size))
        strcpy(data, "(none)");
    printf("%s=%s\n", label, data);
}

int main(void)
{
    char name[256];
    DWORD size = sizeof(name);
    static const char *variables[] = {
        "USERNAME", "USERPROFILE", "APPDATA", "TEMP", "COMPUTERNAME", "ProgramFiles",
        "ALLUSERSPROFILE", "SystemRoot", "windir", "PATH",
    };
    unsigned int i;

    for (i = 0; i < sizeof(variables) / sizeof(variables[0]); i++) variable(variables[i]);
    if (!GetUserNameA(name, &size)) strcpy(name, "(none)");
    printf("user=%s\n", name);
    size = sizeof(name);
    if (!GetComputerNameA(name, &size)) strcpy(name, "(none)");
    printf("computer=%s\n", name);
    value("bios", "HARDWARE\\DESCRIPTION\\System\\BIOS", "SystemManufacturer");
    value("product", "HARDWARE\\DESCRIPTION\\System\\BIOS", "SystemProductName");
    value("processor", "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0", "ProcessorNameString");
    fflush(stdout);
    return 0;
}
