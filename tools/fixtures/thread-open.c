/* Authored x86 NtOpenThread contract fixture. No game input.
 * Output: thread-open.bin, five scalar words per parameter case, then owned
 * worker/child lifetime, access and output-capture observations. No pointers,
 * process/thread IDs or handle values are emitted. Build CHILD for the child.
 */
#include <windows.h>
#include <winternl.h>
typedef LONG(WINAPI *OPEN_THREAD)(PHANDLE,ACCESS_MASK,POBJECT_ATTRIBUTES,CLIENT_ID*);
#ifdef CHILD
void mainCRTStartup(void) {ExitProcess(37);}
#else
static OPEN_THREAD nt_open;
static DWORD values[512], count;
static HANDLE release_event;
static void append(DWORD value) {values[count++]=value;}
static DWORD WINAPI worker(LPVOID unused) {WaitForSingleObject(release_event,INFINITE);return 21;}
static void probe(DWORD access,OBJECT_ATTRIBUTES *attrs,CLIENT_ID *client) {
    HANDLE opened=(HANDLE)0x13572468;
    LONG status;DWORD error,flags=0;
    SetLastError(0x24681357);
    status=nt_open(&opened,access,attrs,client);error=GetLastError();
    append((DWORD)status);append(opened!=(HANDLE)0x13572468);append(opened==NULL);append(error);
    if(status>=0){GetHandleInformation(opened,&flags);CloseHandle(opened);}
    append((flags&HANDLE_FLAG_INHERIT)!=0);
}
void mainCRTStartup(void) {
    OBJECT_ATTRIBUTES attrs={sizeof(attrs)};
    CLIENT_ID client={(HANDLE)GetCurrentProcessId(),(HANDLE)GetCurrentThreadId()}, changed;
    UNICODE_STRING name={0};DWORD tid,code,written;HANDLE thread,opened,file;
    nt_open=(OPEN_THREAD)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"NtOpenThread");
    if(!nt_open)ExitProcess(1);
    probe(THREAD_QUERY_INFORMATION,&attrs,&client);
    changed=client;changed.UniqueProcess=NULL;probe(THREAD_QUERY_INFORMATION,&attrs,&changed);
    changed=client;changed.UniqueThread=NULL;probe(THREAD_QUERY_INFORMATION,&attrs,&changed);
    changed=client;changed.UniqueProcess=(HANDLE)0x7ffffffc;probe(THREAD_QUERY_INFORMATION,&attrs,&changed);
    changed=client;changed.UniqueThread=(HANDLE)0x7ffffffc;probe(THREAD_QUERY_INFORMATION,&attrs,&changed);
    probe(THREAD_QUERY_INFORMATION,&attrs,NULL);
    probe(THREAD_QUERY_INFORMATION,NULL,&client);
    attrs.Length=0;probe(THREAD_QUERY_INFORMATION,&attrs,&client);attrs.Length=sizeof(attrs)-1;probe(THREAD_QUERY_INFORMATION,&attrs,&client);attrs.Length=sizeof(attrs);
    attrs.ObjectName=&name;probe(THREAD_QUERY_INFORMATION,&attrs,&client);attrs.ObjectName=NULL;
    attrs.RootDirectory=(HANDLE)1;probe(THREAD_QUERY_INFORMATION,&attrs,&client);attrs.RootDirectory=NULL;
    attrs.SecurityDescriptor=(PVOID)0x12345;probe(THREAD_QUERY_INFORMATION,&attrs,&client);attrs.SecurityDescriptor=NULL;
    attrs.Attributes=2;probe(THREAD_QUERY_INFORMATION,&attrs,&client);attrs.Attributes=0x40;probe(THREAD_QUERY_INFORMATION,&attrs,&client);attrs.Attributes=0x200;probe(THREAD_QUERY_INFORMATION,&attrs,&client);attrs.Attributes=0xffffffff;probe(THREAD_QUERY_INFORMATION,&attrs,&client);attrs.Attributes=0;
    probe(0,&attrs,&client);probe(GENERIC_READ,&attrs,&client);probe(0x8000,&attrs,&client);
    SetLastError(0x24681357);append((DWORD)nt_open(NULL,THREAD_QUERY_INFORMATION,&attrs,&client));append(GetLastError());
    release_event=CreateEventW(NULL,TRUE,FALSE,NULL);thread=CreateThread(NULL,0,worker,NULL,CREATE_SUSPENDED,&tid);if(!release_event||!thread)ExitProcess(2);
    SetLastError(0x24681357);opened=OpenThread(GENERIC_EXECUTE,FALSE,tid);append(opened!=NULL);append(GetLastError());
    SetLastError(0x24681357);append(SuspendThread(opened));append(GetLastError());
    SetLastError(0x24681357);append(ResumeThread(opened));append(GetLastError());CloseHandle(opened);
    changed=client;changed.UniqueThread=(HANDLE)tid;opened=NULL;
    SetLastError(0x24681357);append((DWORD)nt_open(&opened,THREAD_QUERY_INFORMATION,&attrs,&changed));append(GetLastError());append(opened!=NULL);
    append(GetExitCodeThread(opened,&code));append(code);
    SetLastError(0x24681357);append(ResumeThread(opened));append(GetLastError());
    SetLastError(0x24681357);append(WaitForSingleObject(opened,0));append(GetLastError());
    CloseHandle(opened);SetEvent(release_event);WaitForSingleObject(thread,5000);
    opened=NULL;append((DWORD)nt_open(&opened,THREAD_QUERY_INFORMATION,&attrs,&changed));append(opened!=NULL);if(opened){append(GetExitCodeThread(opened,&code));append(code);CloseHandle(opened);}else{append(0);append(0);}
    CloseHandle(thread);CloseHandle(release_event);
    /* A real suspended child, with the PI thread alias closed before lookup.
     * Opening must locate the live guest process owner, not reuse that alias. */
    {
        STARTUPINFOW startup={sizeof(startup)};PROCESS_INFORMATION child;
        HANDLE query,control,again;DWORD flags;
        if(!CreateProcessW(L"thread-open-child.exe",NULL,NULL,NULL,FALSE,CREATE_SUSPENDED,NULL,NULL,&startup,&child))ExitProcess(4);
        changed.UniqueProcess=(HANDLE)child.dwProcessId;changed.UniqueThread=(HANDLE)child.dwThreadId;
        CloseHandle(child.hThread);
        query=(HANDLE)0x13572468;SetLastError(0x24681357);
        append((DWORD)nt_open(&query,THREAD_QUERY_INFORMATION,&attrs,&changed));append(query!=NULL);append(GetLastError());
        append(GetExitCodeThread(query,&code));append(code);
        SetLastError(0x24681357);append(ResumeThread(query));append(GetLastError());
        SetLastError(0x24681357);append(WaitForSingleObject(query,0));append(GetLastError());
        changed.UniqueProcess=NULL;control=(HANDLE)0x13572468;
        SetLastError(0x24681357);append((DWORD)nt_open(&control,THREAD_QUERY_INFORMATION|THREAD_SUSPEND_RESUME|SYNCHRONIZE,&attrs,&changed));append(control!=NULL);append(GetLastError());
        again=OpenThread(THREAD_QUERY_INFORMATION,TRUE,child.dwThreadId);append(again!=NULL);flags=0;GetHandleInformation(again,&flags);append((flags&HANDLE_FLAG_INHERIT)!=0);CloseHandle(again);
        append(ResumeThread(control));append(WaitForSingleObject(control,5000));append(GetExitCodeThread(control,&code));append(code);
        append(WaitForSingleObject(child.hProcess,5000));
        changed.UniqueProcess=(HANDLE)child.dwProcessId;again=NULL;
        SetLastError(0x24681357);append((DWORD)nt_open(&again,THREAD_QUERY_INFORMATION,&attrs,&changed));append(again!=NULL);append(GetLastError());append(GetExitCodeThread(again,&code));append(code);
        CloseHandle(again);CloseHandle(query);CloseHandle(control);CloseHandle(child.hProcess);
    }
    /* Win32 forwards failure through LastError; NT preserves the TEB field. */
    SetLastError(0x24681357);opened=OpenThread(THREAD_QUERY_INFORMATION,FALSE,0);append(opened==NULL);append(GetLastError());
    SetLastError(0x24681357);opened=OpenThread(0,FALSE,GetCurrentThreadId());append(opened==NULL);append(GetLastError());
    SetLastError(0x24681357);opened=OpenThread(GENERIC_READ,FALSE,GetCurrentThreadId());append(opened!=NULL);append(GetLastError());CloseHandle(opened);
    {
        DWORD old;BYTE unaligned[12],unaligned_attrs[28],unaligned_cid[12];DWORD i;
        HANDLE *output=(HANDLE*)(unaligned+1),*readonly=(HANDLE*)VirtualAlloc(NULL,4096,MEM_COMMIT|MEM_RESERVE,PAGE_READWRITE);
        if(!readonly)ExitProcess(5);*readonly=(HANDLE)0x13572468;VirtualProtect(readonly,4096,PAGE_READONLY,&old);
        SetLastError(0x24681357);append((DWORD)nt_open(readonly,THREAD_QUERY_INFORMATION,&attrs,&client));append(*readonly==(HANDLE)0x13572468);append(GetLastError());VirtualFree(readonly,0,MEM_RELEASE);
        *output=(HANDLE)0x13572468;SetLastError(0x24681357);append((DWORD)nt_open(output,THREAD_QUERY_INFORMATION,&attrs,&client));append(*output!=(HANDLE)0x13572468);append(GetLastError());if(*output!=(HANDLE)0x13572468&&*output)CloseHandle(*output);
        for(i=0;i<sizeof(attrs);i++)unaligned_attrs[i+1]=((BYTE*)&attrs)[i];
        opened=(HANDLE)0x13572468;SetLastError(0x24681357);append((DWORD)nt_open(&opened,THREAD_QUERY_INFORMATION,(OBJECT_ATTRIBUTES*)(unaligned_attrs+1),&client));append(opened==(HANDLE)0x13572468);append(GetLastError());if(opened!=(HANDLE)0x13572468&&opened)CloseHandle(opened);
        for(i=0;i<sizeof(client);i++)unaligned_cid[i+1]=((BYTE*)&client)[i];
        opened=(HANDLE)0x13572468;SetLastError(0x24681357);append((DWORD)nt_open(&opened,THREAD_QUERY_INFORMATION,&attrs,(CLIENT_ID*)(unaligned_cid+1)));append(opened==(HANDLE)0x13572468);append(GetLastError());if(opened!=(HANDLE)0x13572468&&opened)CloseHandle(opened);
    }
    file=CreateFileW(L"thread-open.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,count*4,&written,NULL)||written!=count*4)ExitProcess(3);CloseHandle(file);ExitProcess(0);
}
#endif
