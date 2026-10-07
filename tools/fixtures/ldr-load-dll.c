/* Authored counted-name native loader differential, no game assets. */
#include <windows.h>
#ifdef OWNED_DLL
BOOL WINAPI DllMain(HINSTANCE module,DWORD reason,LPVOID reserved){
 (void)module;(void)reserved;if(reason==DLL_PROCESS_ATTACH)SetLastError(0x24681357);
#ifdef FAIL_DLL
 if(reason==DLL_PROCESS_ATTACH)return FALSE;
#endif
 return TRUE;
}
DWORD WINAPI fixture_value(void){return 17;}
#else
typedef struct {USHORT Length,MaximumLength;PWSTR Buffer;} USTR;
typedef LONG(WINAPI *LDR_LOAD)(PWSTR,PULONG,USTR*,PVOID*);
static DWORD values[20];static WCHAR missing[]=L"fixture-missing.dll";static WCHAR counted[]=L"fixture-owned.dll-extra";static WCHAR failed[]=L"fixture-failed.dll";
void mainCRTStartup(void){
 LDR_LOAD load=(LDR_LOAD)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"LdrLoadDll");
 USTR name;HMODULE module=(HMODULE)0x12345678,other=NULL;DWORD flags=0,written;HANDLE file;FARPROC procedure;
 if(!load)ExitProcess(1);
 name.Length=sizeof(missing)-2;name.MaximumLength=sizeof(missing);name.Buffer=missing;
 SetLastError(0x13572468);values[0]=(DWORD)load(NULL,NULL,&name,(PVOID*)&module);values[1]=(DWORD)module;values[2]=GetLastError();
 name.Length=17*2;name.MaximumLength=sizeof(counted);name.Buffer=counted;
 SetLastError(0x13572468);values[3]=(DWORD)load(NULL,&flags,&name,(PVOID*)&module);values[4]=module!=NULL;values[5]=GetLastError();
 if(values[3] || !module)ExitProcess(2);
 values[6]=GetModuleHandleW(L"fixture-owned.dll")==module;
 procedure=GetProcAddress(module,"fixture_value");values[7]=procedure?((DWORD(WINAPI*)(void))procedure)():0;
 values[8]=(DWORD)load(NULL,NULL,&name,(PVOID*)&other);values[9]=other==module;
 values[10]=FreeLibrary(module);values[11]=GetModuleHandleW(L"fixture-owned.dll")==other;
 values[12]=FreeLibrary(other);values[13]=GetModuleHandleW(L"fixture-owned.dll")==NULL;
 values[14]=name.Length;values[15]=flags;
 name.Length=sizeof(failed)-2;name.MaximumLength=sizeof(failed);name.Buffer=failed;module=(HMODULE)0x12345678;
 SetLastError(0x13572468);values[16]=(DWORD)load(NULL,NULL,&name,(PVOID*)&module);values[17]=(DWORD)module;values[18]=GetLastError();values[19]=GetModuleHandleW(failed)==NULL;
 file=CreateFileW(L"ldr-load-dll.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,sizeof(values),&written,NULL) || written!=sizeof(values))ExitProcess(3);
 CloseHandle(file);ExitProcess(0);
}
#endif
