#include "win32_min.h"
typedef void (__cdecl *Recorder)(DWORD);
__declspec(dllexport) volatile DWORD State;
static void record(DWORD kind, DWORD reason) {
    Recorder output=(Recorder)GetProcAddress(GetModuleHandleA(NULL),"RecordInit");
    if(output) output((kind<<8)|reason);
}
#ifdef DEPENDENCY
__declspec(dllexport) int __cdecl Dependency(void) { return 7; }
BOOL WINAPI DllMain(HMODULE module,DWORD reason,void *reserved) {
    (void)module;(void)reserved;record(3,reason);return 1;
}
#else
__declspec(dllimport) int __cdecl Dependency(void);
__declspec(dllexport) int __cdecl ImportedValue(void) { return Dependency(); }
__declspec(dllexport) DWORD __cdecl ReadState(void) { return State; }
static void WINAPI callback(void *module,DWORD reason,void *reserved) {
    (void)module;(void)reserved;if(reason==1) State+=0x10000;record(2,reason);
}
#pragma section(".tls", long, read, write)
#pragma section(".tls$ZZZ", long, read, write)
#pragma section(".CRT$XLA", long, read)
#pragma section(".CRT$XLB", long, read)
#pragma section(".CRT$XLZ", long, read)
#pragma section(".rdata$T", long, read)
__declspec(allocate(".tls")) DWORD tls_start=37;
__declspec(allocate(".tls$ZZZ")) char tls_end;
DWORD _tls_index;
__declspec(allocate(".CRT$XLA")) TLSCALLBACK tls_first=NULL;
__declspec(allocate(".CRT$XLB")) TLSCALLBACK tls_function=callback;
__declspec(allocate(".CRT$XLZ")) TLSCALLBACK tls_last=NULL;
__declspec(allocate(".rdata$T")) const TLS_DIRECTORY _tls_used={&tls_start,&tls_end,&_tls_index,&tls_first+1,0,0};
#pragma comment(linker,"/include:__tls_used")
#pragma comment(linker,"/include:_tls_function")
BOOL WINAPI DllMain(HMODULE module,DWORD reason,void *reserved) {
    (void)module;(void)reserved;if(reason==1) State+=1;record(1,reason);return 1;
}
#endif
