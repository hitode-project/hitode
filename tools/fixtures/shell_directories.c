/* Authored Shell directory oracle. No input or windows; output contains scalars.
 * Build with build-shell-directories.ps1. All created paths are below the CWD. */
#include <windows.h>
#include <shlobj.h>
void *memset(void *p,int v,size_t n){volatile unsigned char *q=p;while(n--)*q++=(unsigned char)v;return p;}
static DWORD values[128],used;
static WCHAR root[512],path[512];static char ansi[512];
static void record(DWORD v){values[used++]=v;}
static void name(const WCHAR *suffix){UINT i=0,n=0;while(root[i]){path[i]=root[i];i++;}while(suffix[n])path[i++]=suffix[n++];path[i]=0;}
static int create(int variant){
 if(variant==0)return SHCreateDirectory(0,path);
 if(variant==1)return SHCreateDirectoryExW(0,path,0);
 WideCharToMultiByte(CP_ACP,0,path,-1,ansi,sizeof(ansi),0,0);
 return SHCreateDirectoryExA(0,ansi,0);
}
void mainCRTStartup(void){
 DWORD written;HANDLE file;UINT n;
 if(!GetCurrentDirectoryW(400,root))ExitProcess(1);
 for(int variant=0;variant<3;variant++){
  name(variant==0?L"\\dir-plain":variant==1?L"\\dir-wide":L"\\dir-ansi");record(create(variant));record(create(variant));record((GetFileAttributesW(path)&FILE_ATTRIBUTE_DIRECTORY)!=0);
  n=0;while(root[n])n++; /* Temporarily make the owned directory the base. */
  WCHAR saved[512];for(UINT i=0;i<=n;i++)saved[i]=root[i];for(UINT i=0;path[i];i++)root[i]=path[i];UINT end=0;while(path[end])end++;root[end]=0;
  name(L"\\parent\\leaf");record(create(variant));record(create(variant));record((GetFileAttributesW(path)&FILE_ATTRIBUTE_DIRECTORY)!=0);record(RemoveDirectoryW(path));name(L"\\parent");record(RemoveDirectoryW(path));
  name(L"\\file");file=CreateFileW(path,GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);record(file!=INVALID_HANDLE_VALUE);CloseHandle(file);record(create(variant));name(L"\\file\\child");record(create(variant));name(L"\\file");record(DeleteFileW(path));
  if(variant!=2){name(L"\\\x65e5\x672c\\\x8868");record(create(variant));record((GetFileAttributesW(path)&FILE_ATTRIBUTE_DIRECTORY)!=0);record(RemoveDirectoryW(path));name(L"\\\x65e5\x672c");record(RemoveDirectoryW(path));}
  for(UINT i=0;i<=n;i++)root[i]=saved[i];name(variant==0?L"\\dir-plain":variant==1?L"\\dir-wide":L"\\dir-ansi");record(RemoveDirectoryW(path));
 }
 record(SHCreateDirectory(0,L"relative\\path"));record(SHCreateDirectoryExW(0,L"relative\\path",0));record(SHCreateDirectoryExA(0,"relative\\path",0));
 file=CreateFileA("shell-directories.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,used*4,&written,0)||written!=used*4)ExitProcess(250);CloseHandle(file);ExitProcess(0);
}
