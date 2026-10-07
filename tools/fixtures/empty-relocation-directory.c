/* Authored position-independent DLL and native loader oracle. */
#include <windows.h>
#ifdef OWNED_DLL
#pragma section(".fixture",read)
__declspec(allocate(".fixture")) const DWORD relocation_block[2]={0,8};
BOOL WINAPI DllMain(HINSTANCE module,DWORD reason,LPVOID reserved){(void)module;(void)reason;(void)reserved;return TRUE;}
DWORD WINAPI fixture_value(void){return 17;}
#else
typedef struct {USHORT Length,MaximumLength;PWSTR Buffer;} USTR;
typedef LONG(WINAPI *LDR_LOAD)(PWSTR,PULONG,USTR*,PVOID*);
static WCHAR empty[]=L"fixture-empty.dll",none[]=L"fixture-none.dll",stripped[]=L"fixture-stripped.dll";
static DWORD values[19];
void mainCRTStartup(void){
 LDR_LOAD load=(LDR_LOAD)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"LdrLoadDll");
 WCHAR *names[3]={empty,none,stripped};USHORT sizes[3]={sizeof(empty)-2,sizeof(none)-2,sizeof(stripped)-2};
 HANDLE file;DWORD written;unsigned i;void *reservation=VirtualAlloc((void*)0x50000000,65536,MEM_RESERVE,PAGE_NOACCESS);
 if(!load || !reservation)ExitProcess(1);values[0]=1;
 for(i=0;i<3;i++){
  HMODULE module=(HMODULE)0x12345678;USTR name={sizes[i],sizes[i]+2,names[i]};FARPROC proc;DWORD *out=values+1+i*5;
  SetLastError(0x13572468);out[0]=(DWORD)load(NULL,NULL,&name,(PVOID*)&module);out[1]=out[0]?module==(HMODULE)0x12345678:module!=(HMODULE)0x50000000;out[2]=GetLastError();
  if(!out[0]){proc=GetProcAddress(module,"fixture_value");out[3]=proc?((DWORD(WINAPI*)(void))proc)():0;out[4]=FreeLibrary(module);}else{out[3]=0;out[4]=GetModuleHandleW(names[i])==NULL;}
 }
 values[16]=VirtualFree(reservation,0,MEM_RELEASE);
 reservation=VirtualAlloc((void*)0x50000000,65536,MEM_RESERVE,PAGE_NOACCESS);if(!reservation)ExitProcess(3);
 SetLastError(0x13572468);values[17]=LoadLibraryW(stripped)==NULL;values[18]=GetLastError();VirtualFree(reservation,0,MEM_RELEASE);
 file=CreateFileW(L"empty-relocation-directory.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,sizeof(values),&written,NULL) || written!=sizeof(values))ExitProcess(2);CloseHandle(file);ExitProcess(0);
}
#endif
