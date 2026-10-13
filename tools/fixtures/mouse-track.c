/* Authored hidden-window tracking/query/cancel oracle. Never moves the host
 * cursor or sends physical input. Output mouse-track.bin contains scalars. */
#include <windows.h>
typedef BOOL (WINAPI *TRACK)(LPTRACKMOUSEEVENT);
static DWORD values[512],used;
static void record(DWORD value){values[used++]=value;}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","authored tracker",WS_OVERLAPPED,-32000,-32000,32,32,0,0,GetModuleHandleA(0),0);HMODULE common=LoadLibraryA("comctl32.dll");TRACK alias=(TRACK)GetProcAddress(common,"_TrackMouseEvent");if(!w||!alias)ExitProcess(1);
 const DWORD flags[]={TME_QUERY,0,TME_LEAVE,TME_QUERY,TME_HOVER,TME_QUERY,TME_HOVER|TME_LEAVE,TME_QUERY,TME_CANCEL|TME_HOVER|TME_LEAVE,TME_QUERY,4,TME_QUERY};
 for(UINT front=0;front<2;front++)for(UINT i=0;i<sizeof(flags)/sizeof(flags[0]);i++){
  TRACKMOUSEEVENT event={sizeof(event),flags[i],w,HOVER_DEFAULT};MSG message;SetLastError(0);BOOL ok=front?alias(&event):TrackMouseEvent(&event);record(ok!=0);record(ok?0:GetLastError());record(event.dwFlags);record(event.hwndTrack==w);record(event.hwndTrack==0);record(event.dwHoverTime);UINT leaves=0;while(PeekMessageA(&message,w,WM_MOUSELEAVE,WM_MOUSELEAVE,PM_REMOVE))leaves++;record(leaves);
 }
 for(UINT size=0;size<=20;size+=4){TRACKMOUSEEVENT event={size,TME_LEAVE,w,0};SetLastError(0);BOOL ok=TrackMouseEvent(&event);record(ok!=0);record(ok?0:GetLastError());}
 TRACKMOUSEEVENT event={16,TME_LEAVE,(HWND)0xdead,0};SetLastError(0);record(TrackMouseEvent(&event));record(GetLastError());
 DestroyWindow(w);FreeLibrary(common);HANDLE file=CreateFileA("mouse-track.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,used*4,&written,0)||written!=used*4)ExitProcess(2);CloseHandle(file);ExitProcess(0);
}
