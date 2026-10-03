/* Repository-owned x86 Win32 ABI declarations used to compile unmodified MinHook
 * without a Visual C++ CRT installation. This is an ABI header, not an API emulator. */
#ifndef ADV_PATCH_WINDOWS_H
#define ADV_PATCH_WINDOWS_H
#include "string.h"
typedef unsigned char BYTE, UINT8;
typedef signed char INT8;
typedef unsigned short WORD, UINT16;
typedef short INT16;
typedef unsigned int DWORD, UINT, UINT32, ULONG_PTR, DWORD_PTR, SIZE_T;
typedef int BOOL, INT32, LONG;
typedef unsigned long long UINT64, DWORD64;
typedef long long INT64;
typedef void VOID;
typedef void *HANDLE, *HMODULE, *LPVOID;
typedef const void *LPCVOID;
typedef BYTE *LPBYTE;
typedef DWORD *LPDWORD;
typedef const char *LPCSTR;
typedef const unsigned short *LPCWSTR;
#define WINAPI __stdcall
#define API __declspec(dllimport)
#define NULL ((void *)0)
#define TRUE 1
#define FALSE 0
#define INVALID_HANDLE_VALUE ((HANDLE)-1)
#define ERROR_NO_MORE_FILES 18
#define FIELD_OFFSET(type, field) ((LONG)__builtin_offsetof(type, field))
#define MEM_COMMIT 0x1000
#define MEM_RESERVE 0x2000
#define MEM_RELEASE 0x8000
#define MEM_FREE 0x10000
#define PAGE_READONLY 2
#define PAGE_READWRITE 4
#define PAGE_EXECUTE 0x10
#define PAGE_EXECUTE_READ 0x20
#define PAGE_EXECUTE_READWRITE 0x40
#define PAGE_EXECUTE_WRITECOPY 0x80
#define THREAD_SUSPEND_RESUME 2
#define THREAD_GET_CONTEXT 8
#define THREAD_SET_CONTEXT 0x10
#define THREAD_QUERY_INFORMATION 0x40
#define CONTEXT_CONTROL 0x10001
typedef struct {
    DWORD ContextFlags, Dr0, Dr1, Dr2, Dr3, Dr6, Dr7;
    BYTE FloatSave[112];
    DWORD SegGs, SegFs, SegEs, SegDs, Edi, Esi, Ebx, Edx, Ecx, Eax,
        Ebp, Eip, SegCs, EFlags, Esp, SegSs;
    BYTE ExtendedRegisters[512];
} CONTEXT;
typedef struct { LPVOID BaseAddress, AllocationBase; DWORD AllocationProtect;
    SIZE_T RegionSize; DWORD State, Protect, Type; } MEMORY_BASIC_INFORMATION;
API LPVOID WINAPI VirtualAlloc(LPVOID, SIZE_T, DWORD, DWORD);
API BOOL WINAPI VirtualFree(LPVOID, SIZE_T, DWORD);
API BOOL WINAPI VirtualProtect(LPVOID, SIZE_T, DWORD, LPDWORD);
API SIZE_T WINAPI VirtualQuery(LPCVOID, MEMORY_BASIC_INFORMATION *, SIZE_T);
API BOOL WINAPI FlushInstructionCache(HANDLE, LPCVOID, SIZE_T);
API HANDLE WINAPI GetCurrentProcess(void);
API DWORD WINAPI GetCurrentProcessId(void);
API DWORD WINAPI GetCurrentThreadId(void);
API HANDLE WINAPI HeapCreate(DWORD, SIZE_T, SIZE_T);
API BOOL WINAPI HeapDestroy(HANDLE);
API LPVOID WINAPI HeapAlloc(HANDLE, DWORD, SIZE_T);
API LPVOID WINAPI HeapReAlloc(HANDLE, DWORD, LPVOID, SIZE_T);
API BOOL WINAPI HeapFree(HANDLE, DWORD, LPVOID);
API BOOL WINAPI CloseHandle(HANDLE);
API HANDLE WINAPI OpenThread(DWORD, BOOL, DWORD);
API DWORD WINAPI SuspendThread(HANDLE);
API DWORD WINAPI ResumeThread(HANDLE);
API BOOL WINAPI GetThreadContext(HANDLE, CONTEXT *);
API BOOL WINAPI SetThreadContext(HANDLE, const CONTEXT *);
API DWORD WINAPI GetLastError(void);
API void WINAPI SetLastError(DWORD);
API LONG WINAPI InterlockedCompareExchange(volatile LONG *, LONG, LONG);
API LONG WINAPI InterlockedExchange(volatile LONG *, LONG);
API void WINAPI Sleep(DWORD);
API void WINAPI ExitProcess(UINT);
API HMODULE WINAPI GetModuleHandleA(LPCSTR);
API HMODULE WINAPI GetModuleHandleW(LPCWSTR);
API HMODULE WINAPI LoadLibraryA(LPCSTR);
API BOOL WINAPI FreeLibrary(HMODULE);
API LPVOID WINAPI GetProcAddress(HMODULE, LPCSTR);
API HANDLE WINAPI CreateFileA(LPCSTR, DWORD, DWORD, LPVOID, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, LPCVOID, DWORD, LPDWORD, LPVOID);
#endif
