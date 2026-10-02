#ifndef ADV_PATCH_TLHELP_H
#define ADV_PATCH_TLHELP_H
#include "windows.h"
#define TH32CS_SNAPTHREAD 4
typedef struct { DWORD dwSize, cntUsage, th32ThreadID, th32OwnerProcessID;
    LONG tpBasePri, tpDeltaPri; DWORD dwFlags; } THREADENTRY32;
API HANDLE WINAPI CreateToolhelp32Snapshot(DWORD, DWORD);
API BOOL WINAPI Thread32First(HANDLE, THREADENTRY32 *);
API BOOL WINAPI Thread32Next(HANDLE, THREADENTRY32 *);
#endif
