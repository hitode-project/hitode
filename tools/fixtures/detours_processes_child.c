/* Owned child: DLL_PROCESS_ATTACH must see entry stage zero and set the marker
 * before this entry point runs. The artifact is written after the parent exits. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

__declspec(dllexport) volatile DWORD detours_proof[4] = { 0, 0, 0, 0 };

static BOOL contains(const char *value, const char *needle) {
    DWORD at, index;
    for (at = 0; value[at]; at++) {
        for (index = 0; needle[index] && value[at + index] == needle[index]; index++);
        if (!needle[index]) return TRUE;
    }
    return FALSE;
}
void mainCRTStartup(void) {
    DWORD trace[8], written = 0, variant = 0;
    const char *command = GetCommandLineA();
    const char *name = "detours-child-a.bin";
    HANDLE file;
    if (contains(command, "variant-ws")) { variant = 3; name = "detours-child-ws.bin"; }
    else if (contains(command, "variant-as")) { variant = 2; name = "detours-child-as.bin"; }
    else if (contains(command, "variant-w")) { variant = 1; name = "detours-child-w.bin"; }
    detours_proof[0] = 1;
    trace[0] = 0x44435431;
    trace[1] = 1;
    trace[2] = variant;
    trace[3] = detours_proof[1] == 0xd311a77a;
    trace[4] = detours_proof[2];
    trace[5] = contains(command, "detours-owned-token");
    trace[6] = (DWORD)GetModuleHandleA(NULL);
    trace[7] = detours_proof[0];
    Sleep(150);
    file = CreateFileA(name, GENERIC_WRITE, FILE_SHARE_READ, NULL,
        CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(113);
    if (!WriteFile(file, trace, sizeof trace, &written, NULL) || written != sizeof trace) ExitProcess(114);
    CloseHandle(file);
    ExitProcess(trace[3] && trace[4] == 0 && trace[5] ? 0 : 115);
}
