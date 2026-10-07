/* Authored x86 native loader-shutdown contract probe. No sample input.
 * Build OWNED_DLL for the notification recorder. Only scalar effects emitted. */
#include <windows.h>
typedef void(WINAPI *SHUTDOWN)(void);
typedef LONG(WINAPI *TERMINATE)(HANDLE,LONG);
#ifdef OWNED_DLL
static volatile DWORD *effects;
static SHUTDOWN nt_shutdown;
#ifndef MODULE_ID
#define MODULE_ID 1
#endif
static void record(DWORD tag){effects[8+effects[5]++]=tag;}
void WINAPI configure(volatile DWORD *out){effects=out;nt_shutdown=(SHUTDOWN)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"LdrShutdownProcess");}
BOOL WINAPI DllMain(HINSTANCE instance,DWORD reason,LPVOID reserved){
    /* Warm the authored callback region so the JIT differential also executes
     * translated code before its reentrant loader-shutdown call. */
    if(reason==DLL_PROCESS_DETACH){volatile DWORD warm=0;while(warm<4096)warm++;}
    if(reason==DLL_PROCESS_DETACH&&effects){effects[0]++;effects[1]=reserved!=NULL;record(MODULE_ID*10+2);if(MODULE_ID==2){nt_shutdown();effects[3]++;}}
    if(reason==DLL_THREAD_DETACH&&effects)effects[6]++;
    return TRUE;
}
#else
static DWORD words[64],count;
static volatile DWORD effects[64];
static HANDLE ready,release;
static DWORD slot;
static void append(DWORD value){words[count++]=value;}
static void WINAPI fls_callback(void *value){effects[2]++;effects[8+effects[5]++]=100+(DWORD)value;}
static DWORD WINAPI worker(void *unused){FlsSetValue(slot,(void*)2);SetEvent(ready);WaitForSingleObject(release,INFINITE);return 21;}
void mainCRTStartup(void){
    DWORD code,written,i;HANDLE thread,file;HMODULE dll,second;
    SHUTDOWN shutdown=(SHUTDOWN)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"LdrShutdownProcess");
    TERMINATE terminate=(TERMINATE)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),"NtTerminateProcess");
    dll=LoadLibraryW(L"shutdown-owned.dll");if(!shutdown||!terminate||!dll)ExitProcess(2);
    ((void(WINAPI*)(volatile DWORD*))GetProcAddress(dll,"configure"))(effects);
    second=LoadLibraryW(L"shutdown-second.dll");if(!second)ExitProcess(7);
    ((void(WINAPI*)(volatile DWORD*))GetProcAddress(second,"configure"))(effects);
    slot=FlsAlloc(fls_callback);if(slot==FLS_OUT_OF_INDEXES||!FlsSetValue(slot,(void*)1))ExitProcess(3);
    ready=CreateEventW(NULL,TRUE,FALSE,NULL);release=CreateEventW(NULL,TRUE,FALSE,NULL);
    thread=CreateThread(NULL,0,worker,NULL,0,NULL);if(!thread||WaitForSingleObject(ready,5000))ExitProcess(4);
    file=CreateFileW(L"ldr-shutdown.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);if(file==INVALID_HANDLE_VALUE)ExitProcess(5);
    SetLastError(0x13572468);shutdown();append(1);append(GetLastError());append(effects[0]);append(effects[1]);append(effects[2]);
    append(GetExitCodeThread(thread,&code));append(code);append(WaitForSingleObject(thread,0));append(GetModuleHandleW(L"shutdown-owned.dll")==dll);
    SetLastError(0x24681357);shutdown();append(1);append(GetLastError());append(effects[0]);append(effects[2]);
    SetEvent(release);append(WaitForSingleObject(thread,5000));append(GetExitCodeThread(thread,&code));append(code);append(effects[0]);append(effects[2]);
    append(effects[3]);append(effects[5]);append(effects[6]);for(i=0;i<effects[5];i++)append(effects[8+i]);
    if(!WriteFile(file,words,count*4,&written,NULL)||written!=count*4)terminate((HANDLE)-1,6);
    CloseHandle(file);terminate((HANDLE)-1,0);
}
#endif
