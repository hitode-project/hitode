/* Build: i686-w64-mingw32-gcc -O2 -Wall nt_children.c -o nt_children.exe
 * Authored parent/child process and remote memory checks. */
#include <windows.h>
#include <stdio.h>
#include <string.h>
static void check(const char *name, BOOL ok) {
    printf("%s=%s\n",name,ok?"ok":"FAIL"); fflush(stdout);
    if(!ok){printf("error=%lu\n",GetLastError());ExitProcess(1);}
}
int main(void) {
    char exe[MAX_PATH], command[MAX_PATH+32];
    STARTUPINFOA startup={sizeof(startup)};
    PROCESS_INFORMATION child;
    DWORD code, old;
    SIZE_T transferred;
    void *remote;
    char bytes[16]={0};
    HANDLE remote_thread, pipe_read, pipe_write;
    SECURITY_ATTRIBUTES security={sizeof(security),NULL,TRUE};
    CONTEXT context={0};
    MEMORY_BASIC_INFORMATION memory;
    if(strstr(GetCommandLineA(),"--pipe") || strstr(GetCommandLineA(),"--loop")) {
        WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),"ready",5,&code,NULL);
        if(strstr(GetCommandLineA(),"--loop")) for(;;) Sleep(10);
        return 42;
    }
    if(strstr(GetCommandLineA(),"--child")) return 37;
    GetModuleFileNameA(NULL,exe,sizeof(exe));
    snprintf(command,sizeof(command),"\"%s\" --child",exe);
    check("create_process",CreateProcessA(NULL,command,NULL,NULL,TRUE,0,NULL,NULL,&startup,&child));
    check("wait_child",WaitForSingleObject(child.hProcess,30000)==WAIT_OBJECT_0);
    check("child_exit",GetExitCodeProcess(child.hProcess,&code)&&code==37);
    CloseHandle(child.hThread);CloseHandle(child.hProcess);
    check("create_suspended",CreateProcessA(NULL,command,NULL,NULL,TRUE,CREATE_SUSPENDED,NULL,NULL,&startup,&child));
    context.ContextFlags=CONTEXT_INTEGER|CONTEXT_CONTROL;
    check("get_context",GetThreadContext(child.hThread,&context)&&context.Eax!=0);
    check("set_context",SetThreadContext(child.hThread,&context));
    remote=VirtualAllocEx(child.hProcess,NULL,4096,MEM_COMMIT|MEM_RESERVE,PAGE_READWRITE);
    check("remote_allocate",remote!=NULL);
    check("remote_write",WriteProcessMemory(child.hProcess,remote,"version.dll",12,&transferred)&&transferred==12);
    check("remote_read",ReadProcessMemory(child.hProcess,remote,bytes,12,&transferred)&&transferred==12&&!strcmp(bytes,"version.dll"));
    check("remote_query",VirtualQueryEx(child.hProcess,remote,&memory,sizeof(memory))==sizeof(memory)&&memory.State==MEM_COMMIT);
    check("remote_protect",VirtualProtectEx(child.hProcess,remote,4096,PAGE_READONLY,&old)&&old==PAGE_READWRITE);
    remote_thread=CreateRemoteThread(child.hProcess,NULL,0,(LPTHREAD_START_ROUTINE)GetProcAddress(GetModuleHandleA("kernel32.dll"),"LoadLibraryA"),remote,0,NULL);
    check("remote_thread",remote_thread!=NULL);
    check("remote_thread_wait",WaitForSingleObject(remote_thread,30000)==WAIT_OBJECT_0);
    check("remote_load_library",GetExitCodeThread(remote_thread,&code)&&code!=0);
    CloseHandle(remote_thread);
    check("remote_free",VirtualFreeEx(child.hProcess,remote,0,MEM_RELEASE));
    check("resume_child",ResumeThread(child.hThread)==1);
    check("suspended_child_wait",WaitForSingleObject(child.hProcess,30000)==WAIT_OBJECT_0);
    check("suspended_child_exit",GetExitCodeProcess(child.hProcess,&code)&&code==37);
    CloseHandle(child.hThread);CloseHandle(child.hProcess);
    check("create_pipe",CreatePipe(&pipe_read,&pipe_write,&security,0)&&SetHandleInformation(pipe_read,HANDLE_FLAG_INHERIT,0));
    startup.dwFlags=STARTF_USESTDHANDLES;
    startup.hStdOutput=pipe_write;startup.hStdError=pipe_write;
    snprintf(command,sizeof(command),"\"%s\" --pipe",exe);
    check("pipe_child",CreateProcessA(NULL,command,NULL,NULL,TRUE,0,NULL,NULL,&startup,&child));
    CloseHandle(pipe_write);
    check("pipe_read",ReadFile(pipe_read,bytes,5,&code,NULL)&&code==5&&!memcmp(bytes,"ready",5));
    CloseHandle(pipe_read);
    check("pipe_child_wait",WaitForSingleObject(child.hProcess,30000)==WAIT_OBJECT_0);
    check("pipe_child_exit",GetExitCodeProcess(child.hProcess,&code)&&code==42);
    CloseHandle(child.hThread);CloseHandle(child.hProcess);
    check("loop_pipe",CreatePipe(&pipe_read,&pipe_write,&security,0)&&SetHandleInformation(pipe_read,HANDLE_FLAG_INHERIT,0));
    startup.hStdOutput=pipe_write;startup.hStdError=pipe_write;
    snprintf(command,sizeof(command),"\"%s\" --loop",exe);
    check("loop_child",CreateProcessA(NULL,command,NULL,NULL,TRUE,0,NULL,NULL,&startup,&child));
    CloseHandle(pipe_write);
    check("loop_ready",ReadFile(pipe_read,bytes,5,&code,NULL)&&code==5);
    CloseHandle(pipe_read);
    check("running_suspend",SuspendThread(child.hThread)==0);
    context.ContextFlags=CONTEXT_INTEGER|CONTEXT_CONTROL;
    check("running_context",GetThreadContext(child.hThread,&context));
    check("running_resume",ResumeThread(child.hThread)==1);
    check("terminate_child",TerminateProcess(child.hProcess,41));
    check("terminated_wait",WaitForSingleObject(child.hProcess,30000)==WAIT_OBJECT_0);
    check("terminated_exit",GetExitCodeProcess(child.hProcess,&code)&&code==41);
    CloseHandle(child.hThread);CloseHandle(child.hProcess);
    return 0;
}
