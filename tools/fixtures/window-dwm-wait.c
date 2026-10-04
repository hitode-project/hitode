/* Authored hidden queue lifecycle/wait test; no physical input or GPU. */
#include "win32_min.h"
API BOOL WINAPI PeekMessageA(MSG *, HWND, UINT, UINT, UINT);
API DWORD WINAPI GetQueueStatus(UINT);
API DWORD WINAPI MsgWaitForMultipleObjects(DWORD, const HANDLE *, BOOL, DWORD, DWORD);
API HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
typedef struct { MSG msg; DWORD private_value; } NativeMsg;
typedef struct { HANDLE event; DWORD mode, worker_peek, worker_status; } Case;
static Case current;
static DWORD records[512], words;
static void record(DWORD tag, DWORD value) { records[words++]=tag; records[words++]=value; }
static DWORD WINAPI sender(void *unused) {
    NativeMsg message={0}; (void)unused;
    if(current.mode&16) {
        current.worker_peek=PeekMessageA(&message.msg,0,0x400,0x400,0);
        current.worker_status=GetQueueStatus(0x4ff);
    }
    SetEvent(current.event);
    return 0;
}
static DWORD WINAPI run_case(void *parameter) {
    DWORD tag=(DWORD)parameter*100, mode=current.mode, tid, result;
    HANDLE thread; HWND window=0; NativeMsg message={0};
    if(mode&1) {
        window=CreateWindowExA(0x08000080,"AuthoredDwmWait","authored hidden wait",
            0x02cb0000,-32000,-32000,64,64,0,0,GetModuleHandleA(0),0);
        if(!window) ExitProcess(91);
    }
    record(tag,mode);record(tag+1,window!=0);
    record(tag+2,mode&2?PeekMessageA(&message.msg,0,0,0,0):0);
    record(tag+3,mode&4?GetQueueStatus(0x4ff):0);
    current.event=CreateEventA(0,1,0,0);
    thread=CreateThread(0,0,sender,0,0,&tid);
    if(!current.event||!thread||WaitForSingleObject(thread,1000)!=0)ExitProcess(92);
    record(tag+4,WaitForSingleObject(thread,0));
    record(tag+5,MsgWaitForMultipleObjects(1,&current.event,1,10,0x4bf));
    record(tag+6,WaitForSingleObject(current.event,0));
    record(tag+7,GetQueueStatus(0x4ff));
    record(tag+8,MsgWaitForMultipleObjects(1,&current.event,1,0,0x4bf));
    result=PeekMessageA(&message.msg,0,0,0,1);record(tag+9,result);
    record(tag+10,result?message.msg.message:0);record(tag+11,result?message.msg.wparam:0);
    record(tag+12,result?(DWORD)message.msg.lparam:0);record(tag+13,result&&message.msg.hwnd==window);
    record(tag+14,MsgWaitForMultipleObjects(1,&current.event,1,0,0x4bf));
    record(tag+15,current.worker_peek);record(tag+16,current.worker_status);
    CloseHandle(thread);CloseHandle(current.event);if(window)DestroyWindow(window);
    return 0;
}
void mainCRTStartup(void) {
    DWORD modes[]={0,1,3,5,7,17,19},i,tid,wrote;WNDCLASSA wc={0};HANDLE thread,file;
    wc.procedure=DefWindowProcA;wc.instance=GetModuleHandleA(0);wc.name="AuthoredDwmWait";
    if(!RegisterClassA(&wc))ExitProcess(90);
    for(i=0;i<sizeof(modes)/sizeof(modes[0]);i++) {
        current=(Case){0};current.mode=modes[i];
        thread=CreateThread(0,0,run_case,(void *)(i+1),0,&tid);
        if(!thread||WaitForSingleObject(thread,1000)!=0)ExitProcess(93);
        CloseHandle(thread);
    }
    record(9999,0x44574d57);
    file=CreateFileA("window-dwm-wait.bin",0x40000000,0,0,2,0,0);
    if(file==(HANDLE)-1||!WriteFile(file,records,words*4,&wrote,0)||wrote!=words*4)ExitProcess(94);
    CloseHandle(file);ExitProcess(0);
}
