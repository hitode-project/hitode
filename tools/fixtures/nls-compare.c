/* Authored linguistic comparison oracle, no external input or windows.
 * Build with the x86 Windows SDK; output nls-compare.bin contains only scalars. */
#include <windows.h>
static DWORD values[4096],used;
static void record(DWORD value){values[used++]=value;}
void mainCRTStartup(void){
 const char *ascii[][2]={{"asset_1","asset3"},{"ab-c","abc"},{"co'op","coop"},{"Z","a"},{"A","a"},{".file","0file"},{"a b","ab"},{"a","a_"},{"a2","a10"},{"abc","abcd"},{"a@b","a_b"},{"a'b","ab"},{"",""},{"","a"}};
 const DWORD flags[]={0,1,2,4,8,0x1000,0x10001,0x20001};
 SetThreadLocale(0x411);
 for(UINT i=0;i<sizeof(ascii)/sizeof(ascii[0]);i++){
  WCHAR a[32],b[32];MultiByteToWideChar(932,0,ascii[i][0],-1,a,32);MultiByteToWideChar(932,0,ascii[i][1],-1,b,32);
  SetLastError(0x13572468);record(lstrcmpA(ascii[i][0],ascii[i][1]));SetLastError(0x13572468);record(lstrcmpiA(ascii[i][0],ascii[i][1]));SetLastError(0x13572468);record(lstrcmpW(a,b));SetLastError(0x13572468);record(lstrcmpiW(a,b));
  for(UINT f=0;f<sizeof(flags)/sizeof(flags[0]);f++){record(CompareStringA(0x411,flags[f],ascii[i][0],-1,ascii[i][1],-1));record(CompareStringW(0x411,flags[f],a,-1,b,-1));record(CompareStringEx(L"ja-JP",flags[f],a,-1,b,-1,0,0,0));}
 }
 const WCHAR *wide[][2]={{L"\xff76",L"\x30ab"},{L"\x304b",L"\x30ab"},{L"\xff21",L"A"},{L"\xe9",L"e"},{L"\x7981\x6b62",L"\x77e2\x5370"}};
 for(UINT locale=0;locale<2;locale++)for(UINT i=0;i<5;i++)for(UINT f=0;f<sizeof(flags)/sizeof(flags[0]);f++){record(CompareStringW(locale?0x804:0x411,flags[f],wide[i][0],-1,wide[i][1],-1));record(CompareStringEx(locale?L"zh-CN":L"ja-JP",flags[f],wide[i][0],-1,wide[i][1],-1,0,0,0));}
 WCHAR embedded[]={L'a',0,L'b',0};record(CompareStringW(0x411,0,embedded,3,L"a",1));record(CompareStringW(0x411,0,embedded,1,L"a",1));record(CompareStringW(0x411,0,L"",0,L"",0));
 SetLastError(0);record(CompareStringW(0x411,0x80000000,L"a",-1,L"b",-1));record(GetLastError());
 SetLastError(0);record(CompareStringEx(L"not-a-locale",0,L"a",-1,L"b",-1,0,0,0));record(GetLastError());
 HANDLE file=CreateFileA("nls-compare.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,used*4,&written,0)||written!=used*4)ExitProcess(1);CloseHandle(file);ExitProcess(0);
}
