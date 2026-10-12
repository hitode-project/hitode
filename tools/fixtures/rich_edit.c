/* Authored RichEdit class/text lifecycle oracle. No input or desktop UI.
 * Build with build-rich-edit.ps1; rich-edit.bin contains scalars and synthetic text. */
#include <windows.h>
void *memset(void *p,int v,size_t n){volatile unsigned char *q=p;while(n--)*q++=(unsigned char)v;return p;}
static DWORD results[4096],used;
static void record(DWORD value){results[used++]=value;}
static const char *names[]={"RICHEDIT","RichEdit20A","RichEdit20W"};
static const WCHAR *wide_names[]={L"RICHEDIT",L"RichEdit20A",L"RichEdit20W"};
static void classes(void){
 for(int i=0;i<3;i++){
  WNDCLASSEXA c={0};c.cbSize=sizeof(c);
  record(GetClassInfoExA(0,names[i],&c)!=0);
  record(c.style);record(c.cbWndExtra);record(c.hInstance==0);
 }
}
static void text(HWND window,int capacity){
 WCHAR wide[32]={0};char ansi[32]={0};
 record(SendMessageW(window,WM_GETTEXTLENGTH,0,0));
 record(SendMessageA(window,WM_GETTEXTLENGTH,0,0));
 record(SendMessageW(window,WM_GETTEXT,capacity,(LPARAM)wide));
 record(SendMessageA(window,WM_GETTEXT,capacity,(LPARAM)ansi));
 for(int i=0;i<10;i++){record(wide[i]);record((BYTE)ansi[i]);}
}
void mainCRTStartup(void){
 const WCHAR *texts[]={L"ab\ncd",L"ab\rcd",L"ab\r\ncd",L"ab\r\r\ncd",L"ab\n\ncd",L"ab\r\n",L"",L"abcdefghi"};
 classes();HMODULE first=LoadLibraryW(L"riched32.dll");record(first!=0);classes();
 HMODULE second=LoadLibraryA("riched32.dll");record(second==first);record(FreeLibrary(first));classes();
 for(int c=0;c<3;c++)for(int multi=0;multi<2;multi++){
  HWND window=CreateWindowExW(0,wide_names[c],L"ab\ncd",multi?ES_MULTILINE:0,0,0,32,32,HWND_MESSAGE,0,GetModuleHandleW(0),0);
  record(window!=0);record(IsWindowUnicode(window));record(SendMessageW(window,WM_GETDLGCODE,0,0));text(window,32);
  WNDCLASSEXA ca={0};ca.cbSize=sizeof(ca);WNDCLASSEXW cw={0};cw.cbSize=sizeof(cw);
  record(GetClassInfoExA(0,names[c],&ca)!=0);record(GetClassInfoExW(0,wide_names[c],&cw)!=0);
  record(CallWindowProcA(ca.lpfnWndProc,window,WM_SETTEXT,0,(LPARAM)"callback-a"));text(window,32);
  record(CallWindowProcW(cw.lpfnWndProc,window,WM_SETTEXT,0,(LPARAM)L"callback-w"));text(window,32);
  for(int t=0;t<8;t++){record(SendMessageW(window,WM_SETTEXT,0,(LPARAM)texts[t]));text(window,32);}
  record(SendMessageA(window,WM_SETTEXT,0,(LPARAM)"plain"));text(window,32);record(DestroyWindow(window));
 }
 record(FreeLibrary(second));classes();first=LoadLibraryW(L"riched32.dll");record(first!=0);classes();record(FreeLibrary(first));classes();
 for(int repeat=0;repeat<64;repeat++){first=LoadLibraryW(L"riched32.dll");record(first!=0);record(FreeLibrary(first));}classes();
 HANDLE file=CreateFileA("rich-edit.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,results,used*4,&written,0)||written!=used*4)ExitProcess(250);
 CloseHandle(file);ExitProcess(0);
}
