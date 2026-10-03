/* Native USER32 oracle: invisible windows only, no game assets or audio. */
#include "win32_min.h"
API HANDLE WINAPI GetSystemMenu(HWND,BOOL);
API BOOL WINAPI IsMenu(HANDLE);
API BOOL WINAPI DeleteMenu(HANDLE,UINT,UINT);
API BOOL WINAPI InsertMenuItemA(HANDLE,UINT,BOOL,const void *);
API BOOL WINAPI GetMenuItemInfoA(HANDLE,UINT,BOOL,void *);
API UINT WINAPI GetMenuState(HANDLE,UINT,UINT);
API UINT WINAPI GetMenuItemID(HANDLE,int);
API int WINAPI GetMenuItemCount(HANDLE);
API BOOL WINAPI EnableMenuItem(HANDLE,UINT,UINT);
API BOOL WINAPI CheckMenuItem(HANDLE,UINT,UINT);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static DWORD report[128], count, sizes, moves, suppress;
static void emit(DWORD v) { report[count++]=v; }
static LRESULT WINAPI proc(HWND w,UINT m,WPARAM a,LPARAM b) {
    if(m==5) ++sizes;
    if(m==3) ++moves;
    if(m==0x47 && suppress) return 0;
    return DefWindowProcA(w,m,a,b);
}
void mainCRTStartup(void) {
    WNDCLASSA c={0,proc,0,0,0,0,0,0,NULL,"MenuFixture"};
    HWND w; HANDLE menu, reset, file; DWORD written,i,info[12]; char text[8];
    c.instance=GetModuleHandleA(NULL); if(!RegisterClassA(&c)) ExitProcess(2);
    for(i=0;i<4;++i) {
        suppress=i/2; sizes=moves=0;
        w=CreateWindowExA(0,c.name,"hidden",(i&1)?0x80000000:0x02cf0000,-30000,-30000,320,240,0,0,c.instance,NULL);
        if(!w) ExitProcess(3);
        emit(sizes);emit(moves);sizes=moves=0;
        ShowWindow(w,8); emit(sizes);emit(moves); DestroyWindow(w);
    }
    suppress=0;
    w=CreateWindowExA(0,c.name,"hidden",0x02cf0000,20,30,320,240,0,0,c.instance,NULL);
    menu=GetSystemMenu(w,0); emit(menu!=0); emit(GetSystemMenu(w,0)==menu);
    emit(IsMenu(menu)); emit(GetMenuItemCount(menu));
    emit(DeleteMenu(menu,0xf120,0));
    for(i=0;i<12;++i) info[i]=0;
    info[0]=48;info[1]=0x63;info[3]=8;info[4]=0xabc;info[8]=0x12345678;info[9]=(DWORD)"custom";
    emit(InsertMenuItemA(menu,0,1,info)); emit(GetMenuItemCount(menu)); emit(GetMenuItemID(menu,0));
    emit(GetMenuState(menu,0xabc,0)&8);
    for(i=0;i<12;++i) info[i]=0;
    for(i=0;i<8;++i) text[i]='!';
    info[0]=48;info[1]=0x63;info[9]=(DWORD)text;info[10]=4;
    emit(GetMenuItemInfoA(menu,0xabc,0,info));emit(info[3]);emit(info[4]);emit(info[8]);emit(info[10]);emit(*(DWORD *)text);
    emit(EnableMenuItem(menu,0xabc,1));emit(GetMenuState(menu,0xabc,0)&3);
    emit(CheckMenuItem(menu,0xabc,0)); emit(GetMenuState(menu,0xabc,0)&8);
    emit(GetSystemMenu(w,1));emit(IsMenu(menu));
    reset=GetSystemMenu(w,0); emit(IsMenu(reset)); emit(GetMenuItemCount(reset));
    DestroyWindow(w); emit(IsMenu(reset));
    file=CreateFileA("window-menu.bin",0x40000000,0,NULL,2,0,0);
    if(file==0xffffffffu || !WriteFile(file,report,count*4,&written,NULL) || written!=count*4) ExitProcess(4);
    CloseHandle(file); ExitProcess(0);
}
