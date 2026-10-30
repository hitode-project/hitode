/* Authored USER service checks. Windows 11 WoW64 measurements: empty A/W
 * peeks preserve MSG and last error, dispatch sent messages despite filters,
 * skip GETMESSAGE/FOREGROUNDIDLE hooks, and yield even with PM_NOYIELD.
 * Build: i686-w64-mingw32-gcc -O2 -Wall user_services.c -o user_services.exe -luser32 -lwinmm
 * Also build user_services_reference.exe with -DWINE_REFERENCE: Wine differs
 * from Windows on cold queues and the excluded error cases. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>
#include <string.h>
#define CHECK(x) do { if (!(x)) { printf("FAIL line=%u error=%lu\n",__LINE__,GetLastError()); fflush(stdout); ExitProcess(1); } } while(0)
static LONG (WINAPI *alert)(void);
#ifndef WINE_REFERENCE
static void audit(unsigned which, unsigned after) { SetLastError(0x5a17e40 + which*2 + after); alert(); }
#endif
static HWND window;
static HANDLE ready;
static LONG sent, hooks, idles, peers, stop_peer;
static LRESULT CALLBACK window_proc(HWND hwnd, UINT msg, WPARAM w, LPARAM l) {
    if(msg==WM_APP+1) { ++sent; SetLastError(0x76543210); return 0x12345678; }
    return DefWindowProcW(hwnd,msg,w,l);
}
static LRESULT CALLBACK hook(int code, WPARAM w, LPARAM l) { ++hooks; return CallNextHookEx(NULL,code,w,l); }
static LRESULT CALLBACK idle(int code, WPARAM w, LPARAM l) { ++idles; return CallNextHookEx(NULL,code,w,l); }
static DWORD WINAPI sender(void *unused) {
    DWORD_PTR result=0; SetEvent(ready);
    CHECK(SendMessageTimeoutW(window,WM_APP+1,3,4,SMTO_ABORTIFHUNG,5000,&result));
    CHECK(result==0x12345678); return 0;
}
static DWORD WINAPI peer(void *unused) {
    SetEvent(ready);
    while(!InterlockedCompareExchange(&stop_peer,0,0)) { InterlockedIncrement(&peers); SwitchToThread(); }
    return 0;
}
#ifndef WINE_REFERENCE
static DWORD WINAPI cold(void *argument) {
    unsigned which=(unsigned)(UINT_PTR)argument; POINT p; MSG m;
    audit(which,0); SetLastError(0x12345678);
    switch(which) {
    case 0: GetAsyncKeyState(42); break;
    case 1: GetCursorPos(&p); break;
    case 2: CHECK(!GetQueueStatus(0)); break;
    case 3: CHECK(!GetInputState()); break;
    case 4: CHECK(!PeekMessageW(&m,NULL,WM_APP+20,WM_APP+30,PM_REMOVE)); break;
    case 5: CHECK(!GetQueueStatus(0x8000) && GetLastError()==1004); break;
    }
    audit(which,1); return 0;
}
#endif
static void fast_audit(void) {
    BYTE *keys=VirtualAlloc(NULL,4096,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE); POINT *point=(void *)(keys+512); MSG msg;
    CHECK(keys);
    for(unsigned which=0;which<9;which++) {
        for(unsigned pass=0;pass<2;pass++) {
            if(pass) { SetLastError(0x5a17e80+which*2); alert(); }
            for(unsigned i=0;i<(pass?512:32);i++) switch(which) {
            case 0: GetKeyState(42); break;
            case 1: CHECK(GetKeyboardState(keys)); break;
            case 2: GetAsyncKeyState(42); break;
            case 3: CHECK(GetCursorPos(point)); break;
            case 4: CHECK(!PeekMessageW(&msg,NULL,0,0,PM_REMOVE)); break;
            case 5: CHECK(!PeekMessageA(&msg,NULL,0,0,PM_REMOVE)); break;
            case 6: GetQueueStatus(QS_ALLINPUT); break;
            case 7: CHECK(!GetInputState()); break;
            case 8: timeGetTime(); break;
            }
            if(pass) { SetLastError(0x5a17e81+which*2); alert(); }
        }
    }
    VirtualFree(keys,0,MEM_RELEASE);
}
int main(void) {
    WNDCLASSW cls={0}; MSG message,original; POINT point;
    HANDLE thread; unsigned n; HHOOK get,idle_handle;
    alert=(void *)GetProcAddress(GetModuleHandleA("ntdll.dll"),"NtTestAlert"); CHECK(alert);
    #ifndef WINE_REFERENCE
    for(n=0;n<6;n++) { DWORD code; thread=CreateThread(NULL,0,cold,(void *)(UINT_PTR)n,0,NULL); CHECK(thread); CHECK(WaitForSingleObject(thread,5000)==WAIT_OBJECT_0); CHECK(GetExitCodeThread(thread,&code) && !code); CloseHandle(thread); }
    #endif
    cls.lpfnWndProc=window_proc; cls.hInstance=GetModuleHandleW(NULL); cls.lpszClassName=L"UserServices";
    CHECK(RegisterClassW(&cls));
    window=CreateWindowW(cls.lpszClassName,L"",0,0,0,0,0,HWND_MESSAGE,NULL,cls.hInstance,NULL); CHECK(window);
    while(PeekMessageW(&message,NULL,0,0,PM_REMOVE)) {}
    for(n=0;n<8;n++) {
        memset(&message,0xcc,sizeof(message)); original=message; SetLastError(0x12345678);
        CHECK(!(n&1 ? PeekMessageA(n&2?NULL:&message,NULL,n&4?WM_APP+20:0,n&4?WM_APP+30:0,n&2?PM_REMOVE:PM_NOREMOVE) : PeekMessageW(n&2?NULL:&message,NULL,n&4?WM_APP+20:0,n&4?WM_APP+30:0,n&2?PM_REMOVE:PM_NOREMOVE)));
        CHECK(GetLastError()==0x12345678 && !memcmp(&message,&original,sizeof(message)));
    }
    memset(&message,0xcc,sizeof(message)); original=message; SetLastError(0x12345678);
    CHECK(!PeekMessageW(&message,NULL,0,0,0xffffffff) && GetLastError()==0x12345678 && !memcmp(&message,&original,sizeof(message)));
    SetLastError(0x12345678); CHECK(!GetInputState() && GetLastError()==0x12345678);
    CHECK(!GetQueueStatus(0)); CHECK(!GetQueueStatus(QS_ALLINPUT));
    SetLastError(0x12345678); CHECK(GetCursorPos(&point) && GetLastError()==0x12345678);
    #ifndef WINE_REFERENCE
    SetLastError(0x12345678); CHECK(!GetCursorPos(NULL) && GetLastError()==998);
    BYTE *pages=VirtualAlloc(NULL,8192,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE); DWORD protection;
    CHECK(pages && GetCursorPos((POINT *)(pages+1)));
    CHECK(VirtualProtect(pages+4096,4096,PAGE_NOACCESS,&protection));
    *(DWORD *)(pages+4092)=0x12345678; SetLastError(0x12345678);
    CHECK(!GetCursorPos((POINT *)(pages+4092)) && GetLastError()==998 && *(DWORD *)(pages+4092)==0x12345678);
    CHECK(VirtualProtect(pages,4096,PAGE_READONLY,&protection)); SetLastError(0x12345678);
    CHECK(!GetCursorPos((POINT *)pages) && GetLastError()==998); CHECK(VirtualFree(pages,0,MEM_RELEASE));
    SetLastError(0x12345678); CHECK(!GetAsyncKeyState(256) && GetLastError()==87);
    SetLastError(0x12345678); CHECK(!GetQueueStatus(0x8000) && GetLastError()==1004);
    #endif
    get=SetWindowsHookExW(WH_GETMESSAGE,hook,NULL,GetCurrentThreadId());
    idle_handle=SetWindowsHookExW(WH_FOREGROUNDIDLE,idle,NULL,GetCurrentThreadId()); CHECK(get && idle_handle);
    CHECK(!PeekMessageW(&message,NULL,0,0,PM_REMOVE)); CHECK(!hooks && !idles);
    ready=CreateEventW(NULL,TRUE,FALSE,NULL); CHECK(ready);
    thread=CreateThread(NULL,0,sender,NULL,0,NULL); CHECK(thread);
    CHECK(WaitForSingleObject(ready,5000)==WAIT_OBJECT_0);
    for(n=0;n<5000 && !(HIWORD(GetQueueStatus(QS_SENDMESSAGE))&QS_SENDMESSAGE);n++) Sleep(1);
    CHECK(n<5000); memset(&message,0xcc,sizeof(message)); original=message; SetLastError(0x12345678);
    CHECK(!PeekMessageW(&message,NULL,WM_APP+20,WM_APP+30,PM_NOREMOVE));
    CHECK(sent==1 && !hooks && !idles && GetLastError()==0x76543210 && !memcmp(&message,&original,sizeof(message)));
    CHECK(WaitForSingleObject(thread,5000)==WAIT_OBJECT_0); CloseHandle(thread);
    CHECK(PostMessageW(window,WM_APP+2,0,0));
    CHECK((GetQueueStatus(QS_POSTMESSAGE)&0x00080008)==0x00080008);
    CHECK((GetQueueStatus(QS_POSTMESSAGE)&0x00080008)==0x00080000);
    CHECK(!PeekMessageA(&message,NULL,WM_APP+20,WM_APP+30,PM_REMOVE)); CHECK(!hooks);
    CHECK(PeekMessageW(&message,NULL,WM_APP+2,WM_APP+2,PM_REMOVE)); CHECK(hooks==1 && message.message==WM_APP+2);
    CHECK(UnhookWindowsHookEx(get)); CHECK(UnhookWindowsHookEx(idle_handle));
    ResetEvent(ready); thread=CreateThread(NULL,0,peer,NULL,0,NULL); CHECK(thread);
    CHECK(WaitForSingleObject(ready,5000)==WAIT_OBJECT_0); n=peers;
    for(unsigned i=0;i<10000;i++) CHECK(!PeekMessageW(&message,NULL,0,0,PM_REMOVE|PM_NOYIELD));
    CHECK((unsigned)peers>n); InterlockedExchange(&stop_peer,1);
    CHECK(WaitForSingleObject(thread,5000)==WAIT_OBJECT_0); CloseHandle(thread); CloseHandle(ready);
    DWORD start=timeGetTime(); for(n=0;n<10000;n++) CHECK(timeGetTime()>=start);
    #ifndef WINE_REFERENCE
    SetLastError(0x12345678); CHECK(!PeekMessageW(&message,(HWND)0x12345678,0,0,PM_REMOVE) && GetLastError()==1400);
    #endif
    CHECK(PostMessageW(window,WM_APP+3,0,0)); CHECK(PeekMessageA(&message,NULL,WM_APP+3,WM_APP+3,PM_REMOVE) && message.message==WM_APP+3);
    UINT_PTR timer=SetTimer(window,1,10,NULL); CHECK(timer); Sleep(30);
    SetLastError(0x5a17e92); alert();
    PeekMessageW(&message,NULL,WM_TIMER,WM_TIMER,PM_REMOVE);
    SetLastError(0x5a17e93); alert();
    CHECK(KillTimer(window,timer));
    PostQuitMessage(17); CHECK(PeekMessageW(&message,NULL,0,0,PM_REMOVE) && message.message==WM_QUIT && message.wParam==17);
    INPUT input={0}; input.type=INPUT_KEYBOARD; input.ki.wVk=VK_F24;
    CHECK(SendInput(1,&input,sizeof(input))==1);
    CHECK(((USHORT)GetAsyncKeyState(VK_F24)&0x8001)==0x8001);
    CHECK(((USHORT)GetAsyncKeyState(VK_F24)&0x8001)==0x8000);
    input.ki.dwFlags=KEYEVENTF_KEYUP; CHECK(SendInput(1,&input,sizeof(input))==1);
    CHECK(!(GetAsyncKeyState(VK_F24)&0x8000));
    while(PeekMessageW(&message,NULL,0,0,PM_REMOVE)) {}
    fast_audit();
    DestroyWindow(window);
    puts("CHECK user: cold queues, empty peeks, sent messages, hooks, filters, errors, yield, clock");
    return 0;
}
