/*
 * Authored x86 Winsock initialization oracle; no socket or network connection is created.
 * Inputs cover requested versions, sentinel-filled WSADATA, null outputs, nested calls, and worker threads.
 * Checks compare named/ordinal startup, cleanup, process references, and thread-local error APIs.
 * Build from the repository root: ./tools/fixtures/build-winsock.ps1 using LLVM and the Windows SDK.
 * Run fixture_winsock.exe from target/winsock-tests to write winsock-report.bin.
 * Native/guest comparison: cargo test -p adv-runtime --test winsock
 * Undefined last-error results for rejected startup calls are excluded from comparison.
 */
#include "win32_min.h"
typedef struct { WORD version,highest; char description[257],status[129]; WORD sockets,udp; void *vendor; } DATA;
typedef int (WINAPI *STARTUP)(WORD,DATA *);
typedef int (WINAPI *CLEANUP)(void);
typedef int (WINAPI *ERRORGET)(void);
typedef void (WINAPI *ERRORSET)(int);
API HMODULE WINAPI LoadLibraryA(const char *);
API void *WINAPI GetProcAddress(HMODULE,const char *);
API HANDLE WINAPI CreateThread(void *,DWORD,THREADPROC,void *,DWORD,DWORD *);
API DWORD WINAPI GetLastError(void);
API void WINAPI SetLastError(DWORD);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
void *memcpy(void *out,const void *in,unsigned int size) { volatile BYTE *d=out; const BYTE *s=in; while(size--) *d++=*s++; return out; }
void *memset(void *out,int value,unsigned int size) { volatile BYTE *d=out; while(size--) *d++=(BYTE)value; return out; }
static BYTE report[32768]; static DWORD count;
static void emit(DWORD value) { *(DWORD *)(report+count)=value; count+=4; }
static void bytes(const void *data,DWORD size) { const BYTE *p=data; while(size--) report[count++]=*p++; }
static void fill(void *data,DWORD size) { BYTE *p=data; while(size--) *p++=0xcc; }
static CLEANUP worker_cleanup; static ERRORGET worker_get; static ERRORSET worker_set;
static DWORD WINAPI worker(void *unused) {
    (void)unused; worker_set(555); emit((DWORD)worker_get()); emit((DWORD)worker_cleanup()); emit((DWORD)worker_get()); return 7;
}
void mainCRTStartup(void) {
    HMODULE module=LoadLibraryA("ws2_32.dll"); DWORD pass,i,written; HANDLE file; DATA data;
    WORD versions[]={0,1,0x100,0x101,2,0x102,0x202,0x201,3,0xffff};
    for(pass=0;pass<2;++pass) {
        STARTUP startup=(STARTUP)GetProcAddress(module,pass?(const char *)115:"WSAStartup");
        CLEANUP cleanup=(CLEANUP)GetProcAddress(module,pass?(const char *)116:"WSACleanup");
        ERRORGET get=(ERRORGET)GetProcAddress(module,pass?(const char *)111:"WSAGetLastError");
        ERRORSET set=(ERRORSET)GetProcAddress(module,pass?(const char *)112:"WSASetLastError");
        if(!startup||!cleanup||!get||!set) ExitProcess(1);
        SetLastError(123); emit((DWORD)get()); set(456); emit(GetLastError());
        set(777); emit((DWORD)cleanup()); emit((DWORD)get());
        for(i=0;i<10;++i) {
            int result; fill(&data,sizeof(data)); set(888);
            result=startup(versions[i],&data); emit((DWORD)result); emit((DWORD)get()); bytes(&data,sizeof(data));
            if(!result) { emit((DWORD)cleanup()); emit((DWORD)get()); } else { emit(0xeeeeeeee); emit(0xeeeeeeee); }
        }
        set(999); emit((DWORD)startup(0,NULL)); /* Startup failure does not define the thread error. */ emit(0xeeeeeeee);
        set(999); emit((DWORD)startup(0x202,NULL)); emit((DWORD)get());
        /* Nested successes require two releases; failure never adds a reference. */
        emit((DWORD)startup(0x202,&data)); emit((DWORD)startup(0x101,&data));
        emit((DWORD)cleanup()); emit((DWORD)cleanup()); emit((DWORD)cleanup()); emit((DWORD)get());
        /* Library references belong to the process; error codes belong to each thread. */
        emit((DWORD)startup(0x202,&data)); emit((DWORD)startup(0x202,&data));
        worker_cleanup=cleanup; worker_get=get; worker_set=set;
        file=CreateThread(NULL,0,worker,NULL,0,NULL); if(!file) ExitProcess(3);
        set(4444); emit(WaitForSingleObject(file,INFINITE)); emit((DWORD)get()); CloseHandle(file);
        emit((DWORD)cleanup()); emit((DWORD)cleanup()); emit((DWORD)get());
    }
    file=CreateFileA("winsock-report.bin",0x40000000,0,NULL,2,0,0);
    if(file==0xffffffffu||!WriteFile(file,report,count,&written,NULL)||written!=count) ExitProcess(2);
    CloseHandle(file); ExitProcess(0);
}
