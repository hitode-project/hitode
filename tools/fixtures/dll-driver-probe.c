/* Repository-owned input DLL for test_dll_exports.py; build-dll-driver.ps1
 * produces fixture_export_probe.dll. Inputs are raw words and writable buffers;
 * outputs are deterministic scalar values and buffer mutations for ABI/hash
 * comparisons. No sample, device, file, or window is used by the exports. */
#include "win32_min.h"
static volatile DWORD attached;
DWORD __cdecl Fold(BYTE *bytes, DWORD size, DWORD seed) {
    DWORD i; for (i = 0; i < size; ++i) { seed = seed * 1664525u + 1013904223u; bytes[i] ^= (BYTE)(seed >> 24); }
    return seed;
}
DWORD WINAPI Add(DWORD a, DWORD b) { return a + b; }
DWORD __thiscall Advance(DWORD *self, DWORD amount) { *self += amount; return *self; }
DWORD *__cdecl Identity(DWORD *self) { return self; }
unsigned long long __cdecl Wide(DWORD lo, DWORD hi) { return ((unsigned long long)hi << 32) | lo; }
DWORD __cdecl Indirect(DWORD **slot, DWORD amount) { **slot += amount; return **slot; }
DWORD __cdecl NoArgs(void) { return 36 + attached; }
BOOL WINAPI DllMain(HMODULE module, DWORD reason, void *reserved) {
    (void)module; (void)reserved; if (reason == 1) attached = 1; return 1;
}
