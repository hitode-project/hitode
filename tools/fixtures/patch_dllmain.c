#include "patch_headers/windows.h"
#ifndef PATCH_VALUE
#define PATCH_VALUE 33
#endif
static DWORD saved_value;
static BYTE *target;
BOOL WINAPI DllMain(HMODULE self, DWORD reason, LPVOID reserved) {
    DWORD previous, ignored;
    (void)self; (void)reserved;
    if (reason != 1 && reason != 0) return TRUE;
    if (reason == 1) target = GetProcAddress(GetModuleHandleA(NULL), "PatchTarget");
    if (!target || target[0] != 0xb8) return FALSE;
    if (!VirtualProtect(target, 6, PAGE_EXECUTE_READWRITE, &previous)) return FALSE;
    if (reason == 1) saved_value = *(DWORD *)(target + 1);
    *(DWORD *)(target + 1) = reason == 1 ? PATCH_VALUE : saved_value;
    if (!VirtualProtect(target, 6, previous, &ignored)) return FALSE;
    return FlushInstructionCache(GetCurrentProcess(), target, 6);
}
