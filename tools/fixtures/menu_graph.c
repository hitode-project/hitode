/* Authored invisible-window menu graph/resource oracle. No game/device. */
#include "win32_min.h"
API HANDLE WINAPI CreateMenu(void);
API HANDLE WINAPI CreatePopupMenu(void);
API BOOL WINAPI DestroyMenu(HANDLE);
API HANDLE WINAPI GetMenu(HWND);
API BOOL WINAPI SetMenu(HWND,HANDLE);
API BOOL WINAPI DrawMenuBar(HWND);
API HANDLE WINAPI GetSubMenu(HANDLE,int);
API HANDLE WINAPI LoadMenuA(HANDLE,const char *);
API HANDLE WINAPI LoadMenuW(HANDLE,const WORD *);
API HANDLE WINAPI LoadMenuIndirectA(const void *);
API HANDLE WINAPI LoadMenuIndirectW(const void *);
API BOOL WINAPI AppendMenuA(HANDLE,UINT,UINT,const char *);
API BOOL WINAPI AppendMenuW(HANDLE,UINT,UINT,const WORD *);
API BOOL WINAPI InsertMenuA(HANDLE,UINT,UINT,UINT,const char *);
API BOOL WINAPI DeleteMenu(HANDLE,UINT,UINT);
API BOOL WINAPI RemoveMenu(HANDLE,UINT,UINT);
API BOOL WINAPI IsMenu(HANDLE);
API int WINAPI GetMenuItemCount(HANDLE);
API UINT WINAPI GetMenuItemID(HANDLE,int);
API UINT WINAPI GetMenuState(HANDLE,UINT,UINT);
API BOOL WINAPI SetMenuItemInfoW(HANDLE,UINT,BOOL,const void *);
API BOOL WINAPI GetMenuItemInfoW(HANDLE,UINT,BOOL,void *);
API HANDLE WINAPI FindResourceA(HANDLE,const char *,const char *);
API HANDLE WINAPI LoadResource(HANDLE,HANDLE);
API void *WINAPI LockResource(HANDLE);
API WORD WINAPI RegisterClassW(const void *);
API LRESULT WINAPI SendMessageA(HWND,UINT,WPARAM,LPARAM);
API void WINAPI SetLastError(DWORD);
API DWORD WINAPI GetLastError(void);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static DWORD words[512],count,seen_menu,seen_cs_menu,commands;
static void emit(DWORD v) {words[count++]=v;}
static void info(DWORD *p,DWORD mask) {DWORD i;for(i=0;i<12;++i)p[i]=0;p[0]=48;p[1]=mask;}
static LRESULT WINAPI proc(HWND w,UINT m,WPARAM a,LPARAM b) {
    if(m==1) { seen_menu=GetMenu(w);seen_cs_menu=((DWORD *)b)[2]; }
    if(m==0x111) commands=a;
    return DefWindowProcA(w,m,a,b);
}
static HWND window(WORD atom,HANDLE menu,HWND parent,DWORD style) {
    return CreateWindowExA(0,(const char *)(DWORD)atom,"menu",style,-30000,-30000,240,160,parent,menu,GetModuleHandleA(NULL),NULL);
}
static void dump(HANDLE menu,UINT depth) {
    int n=GetMenuItemCount(menu),i;DWORD p[12],j;WORD text[32];HANDLE sub;
    emit(menu!=0);emit(n);
    for(i=0;i<n;++i) {
        sub=GetSubMenu(menu,i);emit(sub!=0);emit(GetMenuItemID(menu,i));
        info(p,0x43);p[9]=(DWORD)text;p[10]=32;
        emit(GetMenuItemInfoW(menu,i,1,p));emit(p[3]);emit(sub && p[4]==sub?0xfeed0001u:p[4]);emit(p[10]);
        for(j=0;j<p[10];++j)emit(text[j]);
        if(sub && depth<8)dump(sub,depth+1);
    }
}
void mainCRTStartup(void) {
    HANDLE root,sub,nested,replacement,loaded,file,res;HWND w,child;DWORD p[12],written;WORD atom,wat;
    WNDCLASSA c={0,proc,0,0,0,0,0,0,NULL,"GraphMenuClass"};
    DWORD wc[10]; static WORD wname[]={'W','I','D','E','_','N','A','M','E',0},wclass[]={'W','i','d','e','C','l','a','s','s',0};
    c.instance=GetModuleHandleA(NULL);atom=RegisterClassA(&c);if(!atom)ExitProcess(2);
    root=CreateMenu();sub=CreatePopupMenu();nested=CreatePopupMenu();emit(IsMenu(root));emit(GetMenuItemCount(root));
    emit(AppendMenuA(nested,0,81,"deep"));emit(AppendMenuA(sub,0,71,"one"));emit(AppendMenuA(sub,0x10,nested,"nest"));
    emit(AppendMenuA(root,0x10,sub,"popup"));emit(AppendMenuA(root,0,72,"two"));
    emit(GetSubMenu(root,0)==sub);emit(GetMenuItemID(root,0));emit(GetMenuState(root,0,0x400));
    info(p,6);emit(GetMenuItemInfoW(root,0,1,p));emit(p[4]==sub);emit(p[5]==sub);
    emit(InsertMenuA(root,71,0,70,"before one"));emit(GetMenuItemCount(root));emit(GetMenuItemCount(sub));emit(GetMenuItemID(sub,0));
    info(p,0x63);p[3]=8;p[4]=82;p[8]=0x1234;p[9]=(DWORD)L"changed";
    emit(SetMenuItemInfoW(root,81,0,p));info(p,0x23);emit(GetMenuItemInfoW(nested,82,0,p));emit(p[3]);emit(p[4]);emit(p[8]);
    w=window(atom,root,0,0);emit(w!=0);emit(GetMenu(w)==root);emit(seen_menu==root);emit(seen_cs_menu==root);
    if(!DrawMenuBar(w)||GetMenu(w)!=root)ExitProcess(5);
    if(DrawMenuBar(0)||GetLastError()!=1400)ExitProcess(6);
    child=window(atom,123,w,0x40000000);emit(child!=0);emit(GetMenu(child));
    SetLastError(0x4242);emit(SetMenu(child,root));emit(GetLastError());DestroyWindow(child);
    replacement=CreateMenu();emit(SetMenu(w,replacement));emit(GetMenu(w)==replacement);emit(IsMenu(root));
    SendMessageA(w,0x111,82,0);emit(commands);DestroyWindow(w);emit(IsMenu(replacement));emit(IsMenu(root));
    emit(RemoveMenu(root,0,0x400));emit(IsMenu(sub));emit(IsMenu(nested));emit(DestroyMenu(root));emit(IsMenu(sub));
    root=CreateMenu();emit(AppendMenuA(root,0x10,sub,"attached"));emit(DeleteMenu(root,0,0x400));emit(IsMenu(sub));emit(IsMenu(nested));DestroyMenu(root);
    root=CreateMenu();sub=CreatePopupMenu();AppendMenuA(root,0x10,sub,"x");emit(DestroyMenu(root));emit(IsMenu(sub));
    SetLastError(0x4242);emit(DestroyMenu(root));emit(GetLastError());
    loaded=LoadMenuA(c.instance,(const char *)101);dump(loaded,0);DestroyMenu(loaded);
    loaded=LoadMenuW(c.instance,(const WORD *)102);dump(loaded,0);DestroyMenu(loaded);
    res=FindResourceA(c.instance,(const char *)101,(const char *)4);
    loaded=LoadMenuIndirectA(LockResource(LoadResource(c.instance,res)));dump(loaded,0);DestroyMenu(loaded);
    res=FindResourceA(c.instance,(const char *)102,(const char *)4);
    loaded=LoadMenuIndirectW(LockResource(LoadResource(c.instance,res)));dump(loaded,0);DestroyMenu(loaded);
    c.name="DefaultMenuClass";c.menu="MENU_STD";atom=RegisterClassA(&c);w=window(atom,0,0,0);emit(w!=0);emit(seen_cs_menu==GetMenu(w));dump(GetMenu(w),0);DestroyWindow(w);
    wc[0]=0;wc[1]=(DWORD)proc;wc[2]=wc[3]=0;wc[4]=c.instance;wc[5]=wc[6]=wc[7]=0;wc[8]=(DWORD)wname;wc[9]=(DWORD)wclass;
    wat=RegisterClassW(wc);emit(wat!=0);wname[0]='X';w=window(wat,0,0,0);emit(w!=0);dump(GetMenu(w),0);DestroyWindow(w);
    SetLastError(0x4242);emit(GetSubMenu(0,0));emit(GetLastError());
    root=CreateMenu();SetLastError(0x4242);emit(GetSubMenu(root,0));emit(GetLastError());
    SetLastError(0x4242);emit(GetSubMenu(root,0xffffffff));emit(GetLastError());DestroyMenu(root);
    SetLastError(0x4242);emit(LoadMenuA(c.instance,"MISSING_MENU"));emit(GetLastError());
    root=CreateMenu();w=window(atom,root,0,0);emit(DestroyMenu(root));emit(GetMenu(w)==root);emit(GetMenu(w)==0);DestroyWindow(w);
    root=CreateMenu();sub=CreatePopupMenu();AppendMenuA(root,0x10,sub,"child");
    emit(DestroyMenu(sub));emit(GetSubMenu(root,0)==sub);emit(GetSubMenu(root,0)==0);emit(GetMenuItemID(root,0));DestroyMenu(root);
    file=CreateFileA("menu-graph.bin",0x40000000,0,NULL,2,0,0);
    if(file==0xffffffffu||!WriteFile(file,words,count*4,&written,NULL)||written!=count*4)ExitProcess(4);
    CloseHandle(file);ExitProcess(0);
}
