/* Authored suspended-child resume and optional-output NT contract. */
#include <windows.h>
#ifdef CHILD
void mainCRTStartup(void){ExitProcess(37);}
#else
typedef LONG(WINAPI *NT_RESUME)(HANDLE,PULONG);
static DWORD values[20];
void mainCRTStartup(void){
    NT_RESUME resume=(NT_RESUME)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"NtResumeThread");
    STARTUPINFOW startup={sizeof(startup)};PROCESS_INFORMATION info;DWORD previous=0x12345678,exitcode=0,written,attempt;HANDLE file;
    if(!resume || !CreateProcessW(L"process-child.exe",NULL,NULL,NULL,FALSE,CREATE_SUSPENDED|CREATE_NO_WINDOW,NULL,NULL,&startup,&info))ExitProcess(1);
    SetLastError(0x13572468);values[0]=(DWORD)resume((HANDLE)0x12345678,&previous);values[1]=previous;values[2]=GetLastError();
    values[3]=(DWORD)resume(info.hThread,(DWORD*)1);values[4]=GetLastError();
    values[5]=SuspendThread(info.hThread);
    values[6]=(DWORD)resume(info.hThread,&previous);values[7]=previous;values[8]=GetLastError();
    values[9]=SuspendThread(info.hThread);
    values[10]=(DWORD)resume(info.hThread,NULL);values[11]=SuspendThread(info.hThread);
    values[12]=ResumeThread(info.hThread);
    for(attempt=0;attempt<128;attempt++){DWORD prior=ResumeThread(info.hThread);if(prior<=1 || prior==(DWORD)-1)break;}
    values[13]=WaitForSingleObject(info.hProcess,5000);
    values[14]=GetExitCodeProcess(info.hProcess,&exitcode);values[15]=exitcode;
    previous=0x12345678;values[16]=(DWORD)resume(info.hThread,&previous);values[17]=previous;
    values[18]=CloseHandle(info.hThread);values[19]=CloseHandle(info.hProcess);
    file=CreateFileW(L"nt-resume-thread.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,sizeof(values),&written,NULL) || written!=sizeof(values))ExitProcess(2);
    CloseHandle(file);ExitProcess(0);
}
#endif
