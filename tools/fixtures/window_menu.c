/* Native USER32 oracle: invisible windows only, no game assets or audio. */
#include "win32_min.h"
API HANDLE WINAPI GetSystemMenu(HWND,BOOL);
API BOOL WINAPI IsMenu(HANDLE);
API BOOL WINAPI DeleteMenu(HANDLE,UINT,UINT);
API BOOL WINAPI InsertMenuItemA(HANDLE,UINT,BOOL,const void *);
API BOOL WINAPI InsertMenuItemW(HANDLE,UINT,BOOL,const void *);
API BOOL WINAPI GetMenuItemInfoA(HANDLE,UINT,BOOL,void *);
API BOOL WINAPI GetMenuItemInfoW(HANDLE,UINT,BOOL,void *);
API UINT WINAPI GetMenuState(HANDLE,UINT,UINT);
API UINT WINAPI GetMenuItemID(HANDLE,int);
API int WINAPI GetMenuItemCount(HANDLE);
API BOOL WINAPI EnableMenuItem(HANDLE,UINT,UINT);
API BOOL WINAPI CheckMenuItem(HANDLE,UINT,UINT);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
API void WINAPI SetLastError(DWORD);
API DWORD WINAPI GetLastError(void);
static DWORD report[512], count, sizes, moves, suppress;
static WORD labels[6][32];
static void emit(DWORD v) { report[count++]=v; }
static void clear_info(DWORD *info) { DWORD i;for(i=0;i<12;++i) info[i]=0;info[0]=48; }
static BOOL insert(HANDLE menu,BOOL wide,DWORD *info) {
    return wide?InsertMenuItemW(menu,0,1,info):InsertMenuItemA(menu,0,1,info);
}
static BOOL query(HANDLE menu,UINT id,BOOL wide,DWORD *info) {
    return wide?GetMenuItemInfoW(menu,id,0,info):GetMenuItemInfoA(menu,id,0,info);
}
static void payloads(HANDLE menu) {
    DWORD wide,old_size,type,info[12],id=0xb00,mixed[3]={0x40,0x80,0x100},i,guard[2];
    for(wide=0;wide<2;++wide) for(old_size=0;old_size<2;++old_size) for(type=0;type<3;++type) {
        clear_info(info);info[0]=old_size?44:48;info[1]=0x32;info[2]=type==0?4:type==1?0x100:0x800;
        info[4]=id;info[8]=0x87654321;info[9]=type==0?1:0x12345678;info[10]=9;
        emit(insert(menu,wide,info));
        clear_info(info);info[0]=old_size?44:48;info[1]=0x32;info[9]=0x12345678;info[10]=9;
        SetLastError(0x42424242);emit(query(menu,id,wide,info));emit(GetLastError());
        emit(info[2]);emit(info[8]);emit(info[9]);emit(info[10]);
        /* Modern bitmap and string queries remain distinct from MIIM_TYPE. */
        clear_info(info);info[1]=0x80;emit(query(menu,id,wide,info));emit(info[11]);
        guard[0]=guard[1]=0x21212121;clear_info(info);info[1]=0x40;info[9]=(DWORD)guard;info[10]=4;
        emit(query(menu,id,wide,info));emit(info[10]);emit(guard[0]);emit(guard[1]);
        ++id;
    }
    for(wide=0;wide<2;++wide) for(i=0;i<3;++i) {
        clear_info(info);info[1]=0x12|mixed[i];info[2]=4;info[4]=id;info[9]=1;
        SetLastError(0x42424242);emit(insert(menu,wide,info));emit(GetLastError());
        clear_info(info);info[1]=0x10|mixed[i];info[9]=0x12345678;info[10]=9;
        SetLastError(0x42424242);emit(query(menu,0xb00,wide,info));emit(GetLastError());
        emit(info[2]);emit(info[9]);emit(info[10]);
    }
    /* UTF-16 custom text, truncation, and a query with no output buffer. */
    {
        WORD text[5]={0x03a9,'x','y','z',0},output[5]={0x2121,0x2121,0x2121,0x2121,0x2121};
        clear_info(info);info[1]=0x52;info[9]=(DWORD)text;info[4]=0xc00;
        emit(insert(menu,1,info)); /* Invalid MIIM_TYPE|MIIM_STRING must not add an item. */
        emit(GetMenuState(menu,0xc00,0));
        info[1]=0x42;emit(insert(menu,1,info));
        clear_info(info);info[1]=0x40;emit(query(menu,0xc00,1,info));emit(info[10]);
        info[9]=(DWORD)output;info[10]=3;emit(query(menu,0xc00,1,info));emit(info[10]);
        emit((DWORD)output[0]|((DWORD)output[1]<<16));emit((DWORD)output[2]|((DWORD)output[3]<<16));
    }
}
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
    w=CreateWindowExA(0,c.name,"payloads",0x02cf0000,-30000,-30000,320,240,0,0,c.instance,NULL);
    menu=GetSystemMenu(w,0);
    for(i=0;i<6;++i) {
        static const DWORD ids[6]={0xf120,0xf010,0xf000,0xf020,0xf030,0xf060};
        clear_info(info);info[1]=0x40;
        emit(query(menu,ids[i],0,info));emit(info[10]!=0);
        info[9]=(DWORD)labels[i];info[10]=32;
        emit(query(menu,ids[i],1,info));emit(info[10]!=0);
    }
    payloads(menu);DestroyWindow(w);emit(IsMenu(menu));
    c.style=0x200;c.name="MenuFixtureNoClose";if(!RegisterClassA(&c)) ExitProcess(5);
    w=CreateWindowExA(0,c.name,"no close",0x02cf0000,-30000,-30000,320,240,0,0,c.instance,NULL);
    menu=GetSystemMenu(w,0);emit(GetMenuItemCount(menu));emit(GetMenuState(menu,0xf060,0));
    for(i=0;i<5;++i) emit(GetMenuItemID(menu,(int)i));
    emit(GetSystemMenu(w,1));emit(IsMenu(menu));reset=GetSystemMenu(w,0);emit(GetMenuItemCount(reset));
    DestroyWindow(w);emit(IsMenu(reset));
    file=CreateFileA("window-menu.bin",0x40000000,0,NULL,2,0,0);
    if(file==0xffffffffu || !WriteFile(file,report,count*4,&written,NULL) || written!=count*4) ExitProcess(4);
    CloseHandle(file);
    /* Locale is separate: guest labels are explicitly virtual en-US, while the
       native oracle's labels use its host language and are not byte-compared. */
    file=CreateFileA("window-menu-labels.bin",0x40000000,0,NULL,2,0,0);
    if(file==0xffffffffu || !WriteFile(file,labels,sizeof(labels),&written,NULL) || written!=sizeof(labels)) ExitProcess(6);
    CloseHandle(file); ExitProcess(0);
}
