/* Authored window property oracle. No input or visible windows.
 * Build with build-window-property.ps1; output is scalar contract observations. */
#define UNICODE
#include <windows.h>
void *memset(void *p,int v,size_t n){volatile unsigned char *q=p;while(n--)*q++=(unsigned char)v;return p;}
static DWORD values[256],used,count,sum,strings,integers,stop,mutate;
static const WCHAR *name=L"ADV_fixture_property_Foo";
static void record(DWORD value){values[used++]=value;}
#define CALL(x) do{SetLastError(0x1234);record((DWORD)(ULONG_PTR)(x));record(GetLastError());}while(0)
static BOOL seen(HWND window,ULONG_PTR key,HANDLE value){
 Sleep(1); /* Exercise a callback continuation that blocks before returning. */
 count++;sum+=(ULONG_PTR)value;
 if(key<65536)integers++;else strings++;
 if(mutate){record((ULONG_PTR)RemovePropW(window,L"callback_key"));record(SetPropW(window,L"new_inside",(HANDLE)22));}
 return stop?0:7;
}
static BOOL CALLBACK wide(HWND w,LPCWSTR key,HANDLE v){return seen(w,(ULONG_PTR)key,v);}
static BOOL CALLBACK ansi(HWND w,LPCSTR key,HANDLE v){return seen(w,(ULONG_PTR)key,v);}
static BOOL CALLBACK wide_ex(HWND w,LPWSTR key,HANDLE v,ULONG_PTR p){record(p==0x87654321);return seen(w,(ULONG_PTR)key,v);}
static BOOL CALLBACK ansi_ex(HWND w,LPSTR key,HANDLE v,ULONG_PTR p){record(p==0x87654321);return seen(w,(ULONG_PTR)key,v);}
static void enumerate(HWND w,int kind){
 count=sum=strings=integers=0;SetLastError(0x1234);
 int result=kind==0?EnumPropsW(w,wide):kind==1?EnumPropsA(w,ansi):kind==2?EnumPropsExW(w,wide_ex,0x87654321):EnumPropsExA(w,ansi_ex,0x87654321);
 record(result);record(GetLastError());record(count);record(sum);record(strings);record(integers);
}
void mainCRTStartup(void){
 HWND w=CreateWindowExW(0,L"STATIC",L"",0,0,0,32,32,HWND_MESSAGE,0,0,0);
 HWND other=CreateWindowExW(0,L"STATIC",L"",0,0,0,32,32,HWND_MESSAGE,0,0,0);
 record(w!=0&&other!=0);enumerate(w,2);CALL(GetPropW(w,name));
 CALL(SetPropW(w,name,(HANDLE)123));record(GlobalFindAtomW(name)!=0);
 CALL(GetPropA(w,"adv_FIXTURE_property_fOO"));CALL(GetPropW(other,name));
 ATOM atom=GlobalAddAtomW(name);record(atom!=0);record((ULONG_PTR)GetPropW(w,MAKEINTATOM(atom)));GlobalDeleteAtom(atom);
 for(int kind=0;kind<4;kind++)enumerate(w,kind);
 CALL(SetPropA(w,"adv_fixture_property_foo",0));enumerate(w,2);CALL(RemovePropW(w,name));record(GlobalFindAtomW(name)!=0);
 CALL(SetPropW(w,0,(HANDLE)2));CALL(GetPropW(w,0));CALL(RemovePropW(w,0));
 CALL(SetPropW(w,MAKEINTATOM(7),(HANDLE)3));enumerate(w,2);CALL(RemovePropA(w,(LPCSTR)7));
 CALL(SetPropW(w,L"",(HANDLE)5));CALL(GetPropW(w,L""));enumerate(w,2);CALL(RemovePropW(w,L""));
 CALL(SetPropW(0,L"missing_property",(HANDLE)5));CALL(GetPropW(0,L"missing_property"));CALL(RemovePropW(0,L"missing_property"));enumerate(0,2);
 for(int i=0;i<5;i++){ATOM found=GlobalFindAtomW(name);record(found!=0);if(found)GlobalDeleteAtom(found);}
 CALL(SetPropW(w,L"callback_key",(HANDLE)19));stop=1;for(int kind=0;kind<4;kind++)enumerate(w,kind);stop=0;
 CALL(RemovePropW(w,L"callback_key"));
 for(int kind=0;kind<4;kind++){SetPropW(w,L"callback_key",(HANDLE)19);mutate=1;enumerate(w,kind);mutate=0;CALL(RemovePropA(w,"new_inside"));}
 CALL(SetPropW(w,L"destroy_key",(HANDLE)29));
 atom=GlobalAddAtomW(L"raw_destroy_key");SetPropW(other,MAKEINTATOM(atom),(HANDLE)41);
 SetPropA(other,"ansi_destroy_key",(HANDLE)43);
 record(DestroyWindow(w));CALL(GetPropW(w,L"destroy_key"));CALL(GetPropW(other,L"destroy_key"));record(DestroyWindow(other));
 record(GlobalFindAtomW(L"raw_destroy_key")!=0);record(GlobalFindAtomW(L"ansi_destroy_key")!=0);GlobalDeleteAtom(atom);
 HANDLE file=CreateFileA("window-property.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,used*4,&written,0)||written!=used*4)ExitProcess(250);
 CloseHandle(file);ExitProcess(0);
}
