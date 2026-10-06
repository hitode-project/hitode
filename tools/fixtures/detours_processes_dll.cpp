// Owned injected DLL. Ordinal 1 is the upstream Detours import-table contract.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#ifdef RESTORE_AFTER_WITH
#include <detours.h>
#endif

extern "C" DWORD WINAPI InjectedOrdinalOne(void) { return 0xd311a77a; }
extern "C" BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    volatile DWORD *proof;
    (void)instance;
    (void)reserved;
    if (reason != DLL_PROCESS_ATTACH) return TRUE;
    proof = (volatile DWORD *)GetProcAddress(GetModuleHandleA(NULL), "detours_proof");
    if (!proof) return FALSE;
    proof[2] = proof[0];
    proof[1] = 0xd311a77a;
#ifdef RESTORE_AFTER_WITH
    proof[3] = DetourRestoreAfterWith();
    if (!proof[3]) return FALSE;
#else
    proof[3] = 1;
#endif
    return TRUE;
}
