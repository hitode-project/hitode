/* Manual, bounded-by-parent native probe. No audio/window/game. Deliberately
 * exits the WinMM callback worker; this misuse is not a compatibility promise.
 * Only probe-timer-worker-exit.ps1 may run it (the synchronous kill can hang). */
#include "win32_min.h"
API UINT WINAPI timeSetEvent(UINT,UINT,void *,DWORD,UINT);
API UINT WINAPI timeKillEvent(UINT);
API void WINAPI ExitThread(DWORD);
API DWORD WINAPI GetCurrentThreadId(void);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
#ifndef SYNC
#define SYNC 0
#endif
static DWORD words[16],count;
static HANDLE started,second;
static volatile DWORD dead_tid,new_tid;
static void record(DWORD word) {
    DWORD written; HANDLE out;
    words[count++]=word;
    out=CreateFileA("report.bin",0x40000000,0,NULL,2,0,0);
    if(out!=0xffffffffu) { WriteFile(out,words,count*4,&written,NULL);CloseHandle(out); }
}
static void WINAPI die(UINT id,UINT message,DWORD user,DWORD one,DWORD two) {
    (void)id;(void)message;(void)user;(void)one;(void)two;
    dead_tid=GetCurrentThreadId();SetEvent(started);ExitThread(0);
}
static void WINAPI again(UINT id,UINT message,DWORD user,DWORD one,DWORD two) {
    (void)id;(void)message;(void)user;(void)one;(void)two;
    new_tid=GetCurrentThreadId();SetEvent(second);
}
void mainCRTStartup(void) {
    UINT id,next;
    started=CreateEventA(NULL,1,0,NULL);second=CreateEventA(NULL,1,0,NULL);
    id=timeSetEvent(20,0,die,0,1|(SYNC ? 0x100 : 0));record(id!=0);
    record(WaitForSingleObject(started,1000));Sleep(50);record(dead_tid!=0);
    record(0xdead0001);
    record(timeKillEvent(id));
    record(0xdead0002);
    next=timeSetEvent(20,0,again,0,0);record(next!=0);
    record(WaitForSingleObject(second,1000));record(new_tid!=0 && new_tid!=dead_tid);
    record(timeKillEvent(next));CloseHandle(started);CloseHandle(second);ExitProcess(0);
}
