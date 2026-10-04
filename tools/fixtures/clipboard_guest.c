/* GUEST ONLY. This fixture must never be launched as a native process: the
   runtime test executes it inside the isolated USER32 model, not host USER32. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
static DWORD words[256],used,failed,drawA,drawB,chainB,destroyed;
static HWND a,b,nextA,nextB;
static void check(BOOL good){words[used++]=good!=0;if(!good)failed++;}
static LRESULT CALLBACK proc(HWND h,UINT message,WPARAM w,LPARAM l){
    if(message==WM_DRAWCLIPBOARD){
        HWND next=h==a?nextA:nextB;
        if(h==a)drawA++;if(h==b)drawB++;
        if(next)SendMessageA(next,message,w,l);return 0;
    }
    if(message==WM_CHANGECBCHAIN){
        HWND *next=h==a?&nextA:&nextB;if(h==b)chainB++;
        if((HWND)w==*next)*next=(HWND)l;else if(*next)SendMessageA(*next,message,w,l);return 0;
    }
    if(message==WM_DESTROYCLIPBOARD){destroyed++;return 0;}
    return DefWindowProcA(h,message,w,l);
}
static DWORD WINAPI opener(LPVOID ignored){(void)ignored;check(OpenClipboard(NULL));return 0;}
void mainCRTStartup(void){
    WNDCLASSA wc={0};DWORD wrote,id;HANDLE file,thread;HGLOBAL h,r,saved;BYTE *p;WCHAR *wide;
    UINT first,second,delay,format,count;BOOL text,unicode,oem,locale;CHAR name[64],layout[9];WCHAR wname[64],wlayout[9];HKL list[2];
    wc.lpfnWndProc=proc;wc.hInstance=GetModuleHandleA(NULL);wc.lpszClassName="GuestClipboardContract";
    check(RegisterClassA(&wc)!=0);
    a=CreateWindowExA(0,wc.lpszClassName,"a",WS_OVERLAPPEDWINDOW,0,0,16,12,NULL,NULL,wc.hInstance,NULL);
    b=CreateWindowExA(0,wc.lpszClassName,"b",WS_OVERLAPPEDWINDOW,0,0,16,12,NULL,NULL,wc.hInstance,NULL);
    check(a!=NULL&&b!=NULL);
    nextA=SetClipboardViewer(a);check(nextA==NULL);check(drawA==1);
    nextB=SetClipboardViewer(b);check(nextB==a);check(drawB==1);check(GetClipboardViewer()==b);
    first=RegisterClipboardFormatA("Rtf");second=RegisterClipboardFormatW(L"Other");delay=RegisterClipboardFormatA("Delayed");
    check(first>=0xc000&&second!=first);check(RegisterClipboardFormatW(L"rTF")==first);
    check(GetClipboardFormatNameA(first,name,64)==3&&name[0]=='R'&&name[2]=='f'&&name[3]==0);
    check(GetClipboardFormatNameW(second,wname,64)==5&&wname[0]=='O'&&wname[5]==0);
    check(OpenClipboard(a));check(GetOpenClipboardWindow()==a);check(EmptyClipboard());check(GetClipboardOwner()==a);
    h=GlobalAlloc(GMEM_MOVEABLE|GMEM_ZEROINIT,16);p=GlobalLock(h);p[0]='r';p[1]=0;GlobalUnlock(h);
    check(SetClipboardData(first,h)==h);check(SetClipboardData(second,h)==h);check(CountClipboardFormats()==2);
    check(GlobalFree(h)==h&&GetLastError()==ERROR_ACCESS_DENIED);check(GlobalReAlloc(h,64,GMEM_MOVEABLE)==NULL);
    check(GetClipboardData(first)==h&&GetClipboardData(second)==h);
    check(CloseClipboard());check(drawA==2&&drawB==2);
    check(OpenClipboard(a));check(EmptyClipboard());check(destroyed==1);check(GlobalFlags(h)==GMEM_INVALID_HANDLE);
    r=GlobalAlloc(GMEM_MOVEABLE|GMEM_ZEROINIT,16);p=GlobalLock(r);p[0]=0x82;p[1]=0xa0;p[2]='X';p[3]=0;GlobalUnlock(r);
    check(SetClipboardData(CF_TEXT,r)==r);
    check(SetClipboardData(delay,NULL)==NULL&&GetLastError()==ERROR_CALL_NOT_IMPLEMENTED);check(!IsClipboardFormatAvailable(delay));
    h=GlobalAlloc(GMEM_MOVEABLE,16);check(SetClipboardData(0x300,h)==NULL&&GetLastError()==ERROR_CALL_NOT_IMPLEMENTED);check(GlobalFree(h)==NULL);
    check(CloseClipboard());check(CountClipboardFormats()==4);check(IsClipboardFormatAvailable(CF_UNICODETEXT));
    check(OpenClipboard(NULL));wide=GlobalLock(GetClipboardData(CF_UNICODETEXT));
    check(wide&&wide[0]==0x3042&&wide[1]=='X'&&wide[2]==0);GlobalUnlock(GetClipboardData(CF_UNICODETEXT));
    p=GlobalLock(GetClipboardData(CF_OEMTEXT));check(p&&p[0]==0x82&&p[1]==0xa0&&p[2]=='X'&&p[3]==0);GlobalUnlock(GetClipboardData(CF_OEMTEXT));
    p=GlobalLock(GetClipboardData(CF_LOCALE));check(p&&*(DWORD*)p==0x411);GlobalUnlock(GetClipboardData(CF_LOCALE));
    text=unicode=oem=locale=FALSE;count=0;format=0;
    while((format=EnumClipboardFormats(format))!=0&&count<32){count++;if(format==CF_TEXT)text=TRUE;if(format==CF_UNICODETEXT)unicode=TRUE;if(format==CF_OEMTEXT)oem=TRUE;if(format==CF_LOCALE)locale=TRUE;}
    check(text&&unicode&&oem&&locale&&count==4&&GetLastError()==0);
    check(SetClipboardData(first,r)==NULL&&GetLastError()==ERROR_ACCESS_DENIED);check(CloseClipboard());
    check(ChangeClipboardChain(a,nextA));check(nextB==NULL&&chainB==1);check(GetClipboardViewer()==b);
    check(OpenClipboard(b));check(DestroyWindow(b));check(GetOpenClipboardWindow()==NULL&&GetClipboardViewer()==NULL);
    check(OpenClipboard(a));saved=GetClipboardData(CF_TEXT);check(saved==r);check(CloseClipboard());
    check(DestroyWindow(a));check(GetClipboardOwner()==NULL);check(OpenClipboard(NULL));
    check(GetClipboardData(CF_TEXT)==saved);check(EmptyClipboard());check(GlobalFlags(saved)==GMEM_INVALID_HANDLE);check(CloseClipboard());
    thread=CreateThread(NULL,0,opener,NULL,0,&id);check(thread!=NULL);check(WaitForSingleObject(thread,INFINITE)==WAIT_OBJECT_0);check(CloseHandle(thread));
    check(OpenClipboard(NULL));check(EmptyClipboard());check(GetClipboardOwner()==NULL);
    h=GlobalAlloc(GMEM_MOVEABLE,16);check(SetClipboardData(first,h)==NULL&&GetLastError()==ERROR_ACCESS_DENIED);check(GlobalFree(h)==NULL);check(CloseClipboard());
    check(GetKeyboardLayout(0)==(HKL)0x04110411);check(GetKeyboardLayout(GetCurrentThreadId())==GetKeyboardLayout(0));
    check(GetKeyboardLayoutList(0,NULL)==1);check(GetKeyboardLayoutList(2,list)==1&&list[0]==GetKeyboardLayout(0));
    check(GetKeyboardLayoutNameA(layout)&&layout[0]=='0'&&layout[4]=='0'&&layout[5]=='4'&&layout[6]=='1'&&layout[7]=='1'&&layout[8]==0);
    check(GetKeyboardLayoutNameW(wlayout)&&wlayout[5]=='4'&&wlayout[8]==0);
    check(!SwapMouseButton(TRUE)&&GetSystemMetrics(SM_SWAPBUTTON));check(SwapMouseButton(FALSE)&&!GetSystemMetrics(SM_SWAPBUTTON));
    file=CreateFileA("clipboard-guest.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    if(file==INVALID_HANDLE_VALUE||!WriteFile(file,words,used*4,&wrote,NULL)||wrote!=used*4)ExitProcess(92);
    CloseHandle(file);ExitProcess(failed?42:0);
}
