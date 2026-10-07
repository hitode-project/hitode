/* Authored suspended child: NT reads and caller-output semantics. */
#include <windows.h>
#ifdef CHILD
void mainCRTStartup(void){ExitProcess(37);}
#else
typedef LONG(WINAPI *NT_READ)(HANDLE,PVOID,PVOID,SIZE_T,PSIZE_T);
static DWORD values[32], input=0x24681357, output[2];
void mainCRTStartup(void){
 NT_READ read=(NT_READ)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"NtReadVirtualMemory");
 STARTUPINFOW startup={sizeof(startup)};PROCESS_INFORMATION info;BYTE *base;HANDLE limited=NULL,file;
 SIZE_T count=0;DWORD old,exitcode=0,written;
 if(!read || !CreateProcessW(L"process-child.exe",NULL,NULL,NULL,FALSE,CREATE_SUSPENDED|CREATE_NO_WINDOW,NULL,NULL,&startup,&info))ExitProcess(1);
 base=(BYTE*)VirtualAllocEx(info.hProcess,NULL,8192,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE);
 if(!base){TerminateProcess(info.hProcess,1);ExitProcess(2);}
 WriteProcessMemory(info.hProcess,base,&input,4,&count);WriteProcessMemory(info.hProcess,base+4092,&input,4,&count);
 SetLastError(0x13572468);values[0]=(DWORD)read(info.hProcess,base,output,4,&count);values[1]=(DWORD)count;values[2]=output[0]==input;values[3]=GetLastError();
 count=0x12345678;values[4]=(DWORD)read((HANDLE)0x12345678,base,output,4,&count);values[5]=(DWORD)count;
 values[6]=DuplicateHandle(GetCurrentProcess(),info.hProcess,GetCurrentProcess(),&limited,PROCESS_VM_READ,FALSE,0);
 values[7]=(DWORD)read(limited,base,output,4,&count);values[8]=(DWORD)count;CloseHandle(limited);
 VirtualProtectEx(info.hProcess,base+4096,4096,PAGE_NOACCESS,&old);output[0]=output[1]=0xa5a5a5a5;count=0x12345678;
 values[9]=(DWORD)read(info.hProcess,base+4092,output,8,&count);values[10]=(DWORD)count;values[11]=output[0];values[12]=output[1];
 count=0x12345678;values[13]=(DWORD)read(info.hProcess,NULL,output,4,&count);values[14]=(DWORD)count;
 count=0x12345678;values[15]=(DWORD)read(info.hProcess,base,NULL,4,&count);values[16]=(DWORD)count;
 count=0x12345678;values[17]=(DWORD)read((HANDLE)0x12345678,NULL,NULL,0,&count);values[18]=(DWORD)count;
 values[19]=(DWORD)read(info.hProcess,base,output,4,NULL);values[20]=output[0]==input;
 SetLastError(0x13572468);values[21]=(DWORD)read(info.hProcess,base,output,4,(SIZE_T*)1);values[22]=GetLastError();
 values[23]=VirtualFreeEx(info.hProcess,base,0,MEM_RELEASE);values[24]=ResumeThread(info.hThread);values[25]=WaitForSingleObject(info.hProcess,5000);
 values[26]=GetExitCodeProcess(info.hProcess,&exitcode);values[27]=exitcode;values[28]=CloseHandle(info.hThread);values[29]=CloseHandle(info.hProcess);
 values[30]=output[0];values[31]=output[1];
 file=CreateFileW(L"nt-read-memory.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,sizeof(values),&written,NULL) || written!=sizeof(values))ExitProcess(3);
 CloseHandle(file);ExitProcess(0);
}
#endif
