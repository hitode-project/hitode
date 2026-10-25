/* Authored PE loaded directly from an ISO.
 * Build: i686-w64-mingw32-gcc -O2 -shared -nostdlib -Wl,--entry,_DllMain@12 volumes_module.c -o volumes_module.dll */
#include <windows.h>
__declspec(dllexport) DWORD VolumeProbe(void) { return 0x5151; }
BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) { return TRUE; }
