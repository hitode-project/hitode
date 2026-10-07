/* Authored child and native NT/Win32 shared VM differential; no game data. */
#include <windows.h>
#ifdef CHILD
void mainCRTStartup(void) { ExitProcess(37); }
#else
typedef LONG (WINAPI *NT_ALLOCATE)(HANDLE,PVOID*,ULONG_PTR,PSIZE_T,ULONG,ULONG);
static DWORD values[38];
void mainCRTStartup(void) {
    NT_ALLOCATE allocate=(NT_ALLOCATE)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"NtAllocateVirtualMemory");
    STARTUPINFOW startup={sizeof(startup)}; PROCESS_INFORMATION info;
    MEMORY_BASIC_INFORMATION memory; PVOID base=NULL, conflict, invalid=NULL;
    SIZE_T size=5001, conflict_size=4096, invalid_size=4096, transferred=0;
    DWORD marker=0x24681357, checked=0, exitcode=0, written; HANDLE file;
    values[0]=CreateProcessW(L"process-child.exe",NULL,NULL,NULL,FALSE,CREATE_SUSPENDED|CREATE_NO_WINDOW,NULL,NULL,&startup,&info);
    if(!allocate || !values[0]) ExitProcess(1);
    SetLastError(0x13572468); values[1]=(DWORD)allocate((HANDLE)0x12345678,&base,0,&size,0x3000,PAGE_READWRITE);
    values[2]=base==NULL; values[3]=size==5001; values[4]=GetLastError();
    SetLastError(0x13572468); values[5]=(DWORD)allocate(info.hProcess,&base,0,&size,0x3000,PAGE_READWRITE);values[31]=GetLastError();
    if(values[5]) {TerminateProcess(info.hProcess,1);ExitProcess(2);}
    values[6]=(DWORD)size;values[7]=((DWORD)base&65535)==0;
    values[8]=VirtualQueryEx(info.hProcess,base,&memory,sizeof(memory)); values[9]=memory.BaseAddress==base;
    values[10]=memory.State;values[11]=memory.Protect;values[12]=memory.Type;
    values[13]=WriteProcessMemory(info.hProcess,base,&marker,4,&transferred); values[14]=(DWORD)transferred;
    values[15]=ReadProcessMemory(info.hProcess,base,&checked,4,&transferred); values[16]=(DWORD)transferred;values[17]=checked==marker;
    conflict=base;values[18]=(DWORD)allocate(info.hProcess,&conflict,0,&conflict_size,0x2000,PAGE_READWRITE);
    values[19]=conflict==base;values[20]=conflict_size==4096;
    values[21]=(DWORD)allocate(info.hProcess,&invalid,0,&invalid_size,0x3000,PAGE_WRITECOPY);
    values[22]=invalid==NULL;values[23]=invalid_size==4096;
    SetLastError(0x13572468); values[32]=(DWORD)allocate(info.hProcess,&invalid,0,&invalid_size,0,PAGE_READWRITE);
    values[33]=invalid==NULL && invalid_size==4096;
    invalid_size=0;values[34]=(DWORD)allocate(info.hProcess,&invalid,0,&invalid_size,0x3000,PAGE_READWRITE);
    invalid_size=4096;values[35]=(DWORD)allocate(info.hProcess,NULL,0,&invalid_size,0x3000,PAGE_READWRITE);
    values[36]=(DWORD)allocate(info.hProcess,&invalid,0,NULL,0x3000,PAGE_READWRITE);values[37]=GetLastError();
    values[24]=VirtualFreeEx(info.hProcess,base,0,MEM_RELEASE);values[25]=VirtualQueryEx(info.hProcess,base,&memory,sizeof(memory));values[26]=memory.State;
    values[27]=ResumeThread(info.hThread);values[28]=WaitForSingleObject(info.hProcess,5000);
    values[29]=GetExitCodeProcess(info.hProcess,&exitcode);values[30]=exitcode;
    CloseHandle(info.hThread);CloseHandle(info.hProcess);
    file=CreateFileW(L"nt-remote-allocation.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,sizeof(values),&written,NULL) || written!=sizeof(values)) ExitProcess(3);
    CloseHandle(file);ExitProcess(0);
}
#endif
