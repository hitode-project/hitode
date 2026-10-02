#include "patch_headers/windows.h"
BOOL WINAPI DllMain(HMODULE self, DWORD reason, LPVOID reserved) {
    BYTE *target;
    DWORD previous, ignored;
    (void)self; (void)reserved;
    if (reason != 1) return TRUE;
    target = GetProcAddress(GetModuleHandleA(NULL), "PatchTarget");
    if (!target || target[0] != 0xb8) return FALSE;
    if (!VirtualProtect(target, 6, PAGE_EXECUTE_READWRITE, &previous)) return FALSE;
    *(DWORD *)(target + 1) = 33;
    if (!VirtualProtect(target, 6, previous, &ignored)) return FALSE;
    return FlushInstructionCache(GetCurrentProcess(), target, 6);
}
