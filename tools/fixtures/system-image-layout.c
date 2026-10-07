/* Authored process-family system image/PEB oracle, no sample code or assets. */
#include <windows.h>
#include <intrin.h>
typedef struct {USHORT Length,MaximumLength;PWSTR Buffer;} USTR;
typedef LONG(WINAPI *LDR_LOAD)(PWSTR,PULONG,USTR*,PVOID*);
static DWORD first_native(void){BYTE *ldr=*(BYTE**)(__readfsdword(0x30)+12);BYTE *node=*(BYTE**)(ldr+28);return *(HMODULE*)(node+8)==GetModuleHandleW(L"ntdll.dll");}
static DWORD exe_excluded(void){BYTE *ldr=*(BYTE**)(__readfsdword(0x30)+12);BYTE *head=ldr+28,*node=*(BYTE**)head;HMODULE exe=GetModuleHandleW(NULL);unsigned count=0;while(node!=head && count++<128){if(*(HMODULE*)(node+8)==exe)return 0;node=*(BYTE**)node;}return node==head;}
static DWORD values[18];
void mainCRTStartup(void){
 HANDLE file;DWORD count,info[2];HMODULE native=GetModuleHandleW(L"ntdll.dll");LDR_LOAD load=(LDR_LOAD)GetProcAddress(native,"LdrLoadDll");
#ifdef CHILD
 HMODULE module=NULL;WCHAR text[]=L"fixture-owned.dll";USTR name={sizeof(text)-2,sizeof(text),text};FARPROC proc;
 file=CreateFileW(L"system-image-input.bin",GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL);
 if(file==INVALID_HANDLE_VALUE || !ReadFile(file,info,sizeof(info),&count,NULL) || count!=sizeof(info))ExitProcess(2);CloseHandle(file);
 values[0]=(DWORD)native==info[0];values[1]=(DWORD)load==info[1];values[2]=first_native();values[3]=exe_excluded();
 load=(LDR_LOAD)info[1];SetLastError(0x13572468);values[4]=(DWORD)load(NULL,NULL,&name,(PVOID*)&module);values[5]=module!=NULL;values[7]=GetLastError();
 if(values[4] || !module)ExitProcess(3);proc=GetProcAddress(module,"fixture_value");values[6]=proc?((DWORD(WINAPI*)(void))proc)():0;
 file=CreateFileW(L"system-image-child.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,8*4,&count,NULL) || count!=8*4)ExitProcess(4);CloseHandle(file);ExitProcess(37);
#else
 STARTUPINFOW startup={sizeof(startup)};PROCESS_INFORMATION child;DWORD code;WORD magic=0;SIZE_T transferred=0;
 values[0]=GetSystemMetrics(SM_CXSCREEN)>0;values[1]=first_native();values[2]=exe_excluded();info[0]=(DWORD)native;info[1]=(DWORD)load;
 file=CreateFileW(L"system-image-input.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE || !WriteFile(file,info,sizeof(info),&count,NULL) || count!=sizeof(info))ExitProcess(2);CloseHandle(file);
 values[3]=CreateProcessW(L"process-child.exe",NULL,NULL,NULL,FALSE,CREATE_SUSPENDED|CREATE_NO_WINDOW,NULL,NULL,&startup,&child);if(!values[3])ExitProcess(3);
 values[16]=ReadProcessMemory(child.hProcess,native,&magic,sizeof(magic),&transferred) && magic==0x5a4d && transferred==sizeof(magic);values[17]=ResumeThread(child.hThread);
 values[4]=WaitForSingleObject(child.hProcess,5000);GetExitCodeProcess(child.hProcess,&code);values[5]=code;
 file=CreateFileW(L"system-image-child.bin",GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL);
 if(file==INVALID_HANDLE_VALUE || !ReadFile(file,values+6,8*4,&count,NULL))ExitProcess(4);values[15]=count==8*4;CloseHandle(file);
 values[14]=CloseHandle(child.hThread) && CloseHandle(child.hProcess);
 file=CreateFileW(L"system-image-layout.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,sizeof(values),&count,NULL) || count!=sizeof(values))ExitProcess(5);CloseHandle(file);ExitProcess(0);
#endif
}
