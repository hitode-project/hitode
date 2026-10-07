/* Owned suspended child and native memory write contract probe. */
#include <windows.h>
#ifdef CHILD
void mainCRTStartup(void){ExitProcess(37);}
#else
typedef LONG(WINAPI *NT_WRITE)(HANDLE,PVOID,PVOID,SIZE_T,PSIZE_T);
static DWORD values[36];
static DWORD input[2]={0x24681357,0x76543210};
void mainCRTStartup(void){
    NT_WRITE write=(NT_WRITE)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"NtWriteVirtualMemory");
    STARTUPINFOW startup={sizeof(startup)};PROCESS_INFORMATION info;
    BYTE *base;DWORD checked=0,old,exitcode=0,written;SIZE_T count=0;HANDLE limited=NULL,file;
    if(!write || !CreateProcessW(L"process-child.exe",NULL,NULL,NULL,FALSE,CREATE_SUSPENDED|CREATE_NO_WINDOW,NULL,NULL,&startup,&info))ExitProcess(1);
    base=(BYTE*)VirtualAllocEx(info.hProcess,NULL,8192,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE);if(!base){TerminateProcess(info.hProcess,1);ExitProcess(2);}
    SetLastError(0x13572468);values[0]=(DWORD)write(info.hProcess,base,input,4,&count);values[1]=(DWORD)count;values[2]=GetLastError();
    values[3]=ReadProcessMemory(info.hProcess,base,&checked,4,&count);values[4]=checked==input[0];
    count=0x12345678;SetLastError(0x13572468);values[5]=(DWORD)write((HANDLE)0x12345678,base,input,4,&count);values[6]=(DWORD)count;values[7]=GetLastError();
    values[8]=DuplicateHandle(GetCurrentProcess(),info.hProcess,GetCurrentProcess(),&limited,PROCESS_VM_WRITE,FALSE,0);
    count=0x12345678;values[9]=(DWORD)write(limited,base,input,4,&count);values[10]=(DWORD)count;CloseHandle(limited);
    values[11]=VirtualProtectEx(info.hProcess,base,4096,PAGE_READONLY,&old);count=0x12345678;
    values[12]=(DWORD)write(info.hProcess,base,input+1,4,&count);values[13]=(DWORD)count;
    values[14]=ReadProcessMemory(info.hProcess,base,&checked,4,&count);values[15]=checked==input[0];
    VirtualProtectEx(info.hProcess,base,4096,PAGE_READWRITE,&old);VirtualProtectEx(info.hProcess,base+4096,4096,PAGE_NOACCESS,&old);
    count=0x12345678;values[16]=(DWORD)write(info.hProcess,base+4092,input,8,&count);values[17]=(DWORD)count;
    values[18]=ReadProcessMemory(info.hProcess,base+4092,&checked,4,&count);values[19]=checked==input[0];
    count=0x12345678;values[20]=(DWORD)write(info.hProcess,base,NULL,4,&count);values[21]=(DWORD)count;
    count=0x12345678;values[22]=(DWORD)write(info.hProcess,NULL,input,4,&count);values[23]=(DWORD)count;
    count=0x12345678;values[24]=(DWORD)write((HANDLE)0x12345678,NULL,NULL,0,&count);values[25]=(DWORD)count;
    values[26]=(DWORD)write(info.hProcess,base,input,4,NULL);
    SetLastError(0x13572468);values[27]=(DWORD)write(info.hProcess,base,input,4,(SIZE_T*)1);values[28]=GetLastError();
    values[29]=VirtualFreeEx(info.hProcess,base,0,MEM_RELEASE);values[30]=ResumeThread(info.hThread);values[31]=WaitForSingleObject(info.hProcess,5000);
    values[32]=GetExitCodeProcess(info.hProcess,&exitcode);values[33]=exitcode;
    values[34]=CloseHandle(info.hThread);values[35]=CloseHandle(info.hProcess);
    file=CreateFileW(L"nt-write-memory.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,sizeof(values),&written,NULL) || written!=sizeof(values))ExitProcess(3);
    CloseHandle(file);ExitProcess(0);
}
#endif
