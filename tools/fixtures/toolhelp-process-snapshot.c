/* Authored x86 process-family snapshot and API validation oracle. */
#include <windows.h>
#include <tlhelp32.h>
#ifdef CHILD
void mainCRTStartup(void){ExitProcess(37);}
#else
static DWORD values[40];
static union {PROCESSENTRY32 entry;BYTE bytes[600];} a;
static union {PROCESSENTRY32W entry;BYTE bytes[600];} w;
static HEAPLIST32 heap;
static BOOL find_a(HANDLE snapshot,DWORD pid){
 unsigned limit=0;a.entry.dwSize=sizeof(a.entry);
 if(!Process32First(snapshot,&a.entry))return FALSE;
 do{if(a.entry.th32ProcessID==pid)return TRUE;}while(++limit<4096&&Process32Next(snapshot,&a.entry));return FALSE;
}
static BOOL find_w(HANDLE snapshot,DWORD pid){
 unsigned limit=0;w.entry.dwSize=sizeof(w.entry);
 if(!Process32FirstW(snapshot,&w.entry))return FALSE;
 do{if(w.entry.th32ProcessID==pid)return TRUE;}while(++limit<4096&&Process32NextW(snapshot,&w.entry));return FALSE;
}
void mainCRTStartup(void){
 STARTUPINFOW startup={0};PROCESS_INFORMATION child={0};HANDLE old,fresh,empty,all,file;DWORD written,code,pid=GetCurrentProcessId();BOOL found;unsigned limit=0;
 startup.cb=sizeof(startup);values[0]=sizeof(PROCESSENTRY32);values[1]=sizeof(PROCESSENTRY32W);
 if(!CreateProcessW(L"process-child.exe",NULL,NULL,NULL,FALSE,CREATE_SUSPENDED,NULL,NULL,&startup,&child))ExitProcess(1);
 old=CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS,0);values[2]=old!=INVALID_HANDLE_VALUE;
 values[3]=find_a(old,pid);values[4]=find_a(old,child.dwProcessId);values[5]=a.entry.th32ParentProcessID==pid;
 values[6]=a.entry.cntThreads==1;values[7]=lstrcmpA(a.entry.szExeFile,"process-child.exe")==0;
 if(ResumeThread(child.hThread)==(DWORD)-1||WaitForSingleObject(child.hProcess,5000)!=WAIT_OBJECT_0){TerminateProcess(child.hProcess,2);ExitProcess(2);}
 GetExitCodeProcess(child.hProcess,&code);if(code!=37)ExitProcess(3);CloseHandle(child.hThread);CloseHandle(child.hProcess);
 values[8]=find_a(old,child.dwProcessId);values[9]=find_w(old,child.dwProcessId);
 fresh=CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS,0);values[10]=!find_a(fresh,child.dwProcessId);
 values[11]=find_a(fresh,pid)&&lstrcmpA(a.entry.szExeFile,"fixture.exe")==0;
 empty=CreateToolhelp32Snapshot(0,0);a.entry.dwSize=sizeof(a.entry);values[12]=Process32First(empty,&a.entry);values[13]=GetLastError();
 values[14]=Process32First(INVALID_HANDLE_VALUE,&a.entry);values[15]=GetLastError();
 a.entry.dwSize=sizeof(a.entry)-1;Process32First(fresh,&a.entry);values[16]=GetLastError();Process32Next(fresh,&a.entry);values[17]=GetLastError();
 w.entry.dwSize=sizeof(w.entry)-1;Process32FirstW(fresh,&w.entry);values[18]=GetLastError();Process32NextW(fresh,&w.entry);values[19]=GetLastError();
 a.entry.dwSize=sizeof(a.entry)+4;values[20]=Process32First(fresh,&a.entry);
 w.entry.dwSize=sizeof(w.entry)+4;values[21]=Process32FirstW(fresh,&w.entry);
 all=CreateToolhelp32Snapshot(TH32CS_SNAPALL,0);values[22]=all!=INVALID_HANDLE_VALUE;heap.dwSize=sizeof(heap);found=Heap32ListFirst(all,&heap);
 while(found){if(heap.th32HeapID==(ULONG_PTR)GetProcessHeap()&&heap.th32ProcessID==pid&&(heap.dwFlags&HF32_DEFAULT)){values[23]=1;break;}found=++limit<4096&&Heap32ListNext(all,&heap);}
 heap.dwSize=sizeof(heap)-1;Heap32ListFirst(all,&heap);values[24]=GetLastError();Heap32ListNext(all,&heap);values[25]=GetLastError();
 a.entry.dwSize=sizeof(a.entry);SetLastError(0x13572468);Process32First(fresh,&a.entry);values[26]=GetLastError();
 w.entry.dwSize=sizeof(w.entry);SetLastError(0x13572468);Process32FirstW(fresh,&w.entry);values[27]=GetLastError();
 find_a(empty,pid);values[28]=GetLastError();Process32FirstW(INVALID_HANDLE_VALUE,&w.entry);values[29]=GetLastError();
 for(limit=0;limit<sizeof(a.bytes);limit++)a.bytes[limit]=0xcc;a.entry.dwSize=sizeof(a.entry);Process32First(fresh,&a.entry);values[32]=a.bytes[sizeof(a.entry)-1]==0xcc;
 a.entry.dwSize=sizeof(a.entry)+4;Process32First(fresh,&a.entry);values[33]=a.entry.dwSize==sizeof(a.entry)+4;
 for(limit=0;limit<sizeof(w.bytes);limit++)w.bytes[limit]=0xcc;w.entry.dwSize=sizeof(w.entry);Process32FirstW(fresh,&w.entry);values[34]=w.bytes[sizeof(w.entry)-1]==0;
 for(limit=0;limit<sizeof(w.bytes);limit++)w.bytes[limit]=0xcc;w.entry.dwSize=sizeof(w.entry);Process32FirstW(empty,&w.entry);values[35]=w.entry.th32ProcessID==0xcccccccc;values[36]=w.entry.th32ProcessID==0;
 for(limit=0;limit<sizeof(a.bytes);limit++)a.bytes[limit]=0xcc;a.entry.dwSize=sizeof(a.entry);Process32First(empty,&a.entry);values[37]=a.entry.th32ProcessID==0;values[38]=a.entry.szExeFile[0]==0;values[39]=a.bytes[sizeof(a.entry)-1]==0xcc;
 values[30]=CloseHandle(all)&&CloseHandle(fresh)&&CloseHandle(empty);values[31]=CloseHandle(old);
 file=CreateFileW(L"toolhelp-process-snapshot.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,sizeof(values),&written,NULL)||written!=sizeof(values))ExitProcess(4);
 CloseHandle(file);ExitProcess(0);
}
#endif
