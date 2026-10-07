/* Authored NT delay oracle; no sample code or assets. */
#include <windows.h>
typedef LONG(WINAPI *NT_DELAY)(BYTE,LARGE_INTEGER*);
static DWORD values[18];static volatile LONG progressed;
static DWORD WINAPI worker(void *unused){(void)unused;Sleep(20);InterlockedExchange(&progressed,1);return 37;}
void mainCRTStartup(void){
 NT_DELAY delay=(NT_DELAY)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"NtDelayExecution");
 LARGE_INTEGER interval;HANDLE thread,file;DWORD begin,written,code,status;FILETIME now;DWORD aligned[3]={0,0,0};
 if(!delay)ExitProcess(1);
 SetLastError(0x13572468);values[0]=(DWORD)delay(0,NULL);values[1]=GetLastError();
 SetLastError(0x13572468);values[2]=(DWORD)delay(0,(LARGE_INTEGER*)1);values[3]=GetLastError();
 interval.QuadPart=0;SetLastError(0x13572468);status=(DWORD)delay(0,&interval);values[4]=status==0 || status==0x40000024;values[5]=GetLastError();
 interval.QuadPart=1;SetLastError(0x13572468);values[6]=(DWORD)delay(0,&interval);values[7]=GetLastError();
 thread=CreateThread(NULL,0,worker,NULL,0,NULL);if(!thread)ExitProcess(2);
 interval.QuadPart=-1000000;begin=GetTickCount();SetLastError(0x13572468);values[8]=(DWORD)delay(0,&interval);values[9]=GetLastError();values[10]=GetTickCount()-begin>=80;values[11]=progressed==1;
 values[12]=WaitForSingleObject(thread,2000);GetExitCodeThread(thread,&code);values[13]=code;CloseHandle(thread);
 GetSystemTimeAsFileTime(&now);interval.LowPart=now.dwLowDateTime;interval.HighPart=now.dwHighDateTime;interval.QuadPart+=500000;
 SetLastError(0x13572468);values[14]=(DWORD)delay(0,&interval);values[15]=GetLastError();
 SetLastError(0x13572468);status=(DWORD)delay(0,(LARGE_INTEGER*)(aligned+1));values[16]=status==0 || status==0x40000024;values[17]=GetLastError();
 file=CreateFileW(L"nt-delay-execution.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,sizeof(values),&written,NULL) || written!=sizeof(values))ExitProcess(3);
 CloseHandle(file);ExitProcess(0);
}
