/* Build: i686-w64-mingw32-gcc -O2 -o services.exe services.c */
#include <windows.h>
#include <winternl.h>
#include <stdio.h>
#include <stdint.h>
typedef LONG (WINAPI *alloc_t)(HANDLE,void **,ULONG,SIZE_T *,ULONG,ULONG);
typedef LONG (WINAPI *free_t)(HANDLE,void **,SIZE_T *,ULONG);
typedef LONG (WINAPI *query_t)(HANDLE,PIO_STATUS_BLOCK,void *,ULONG,ULONG);
static void benchmark_system_calls(void) {
    HMODULE nt=GetModuleHandleA("ntdll.dll");
    alloc_t alloc=(alloc_t)GetProcAddress(nt,"NtAllocateVirtualMemory");
    free_t release=(free_t)GetProcAddress(nt,"NtFreeVirtualMemory");
    query_t query=(query_t)GetProcAddress(nt,"NtQueryInformationFile");
    HANDLE objects[2000],file; LARGE_INTEGER start,end,freq,position; IO_STATUS_BLOCK io; char path[MAX_PATH];
    void *pages[2000]; SIZE_T size; unsigned i; LONG status;
    QueryPerformanceFrequency(&freq);
    for(i=0;i<2000;i++) objects[i]=CreateEventW(NULL,TRUE,FALSE,NULL);
    QueryPerformanceCounter(&start);
    for(i=0;i<2000;i++) if(!CloseHandle(objects[i])) ExitProcess(1);
    QueryPerformanceCounter(&end);
    printf("NtClose %.1f ns/call\n",(double)(end.QuadPart-start.QuadPart)*1e9/freq.QuadPart/2000);
    QueryPerformanceCounter(&start);
    for(i=0;i<2000;i++) { pages[i]=NULL; size=4096; status=alloc((HANDLE)-1,&pages[i],0,&size,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE); if(status) ExitProcess(2); }
    QueryPerformanceCounter(&end);
    printf("NtAllocateVirtualMemory %.1f ns/call\n",(double)(end.QuadPart-start.QuadPart)*1e9/freq.QuadPart/2000);
    QueryPerformanceCounter(&start);
    for(i=0;i<2000;i++) { size=0; if(release((HANDLE)-1,&pages[i],&size,MEM_RELEASE)) ExitProcess(3); }
    QueryPerformanceCounter(&end);
    printf("NtFreeVirtualMemory %.1f ns/call\n",(double)(end.QuadPart-start.QuadPart)*1e9/freq.QuadPart/2000);
    GetModuleFileNameA(NULL,path,MAX_PATH);
    file=CreateFileA(path,GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL);
    QueryPerformanceCounter(&start);
    for(i=0;i<20000;i++) if(query(file,&io,&position,8,14)) ExitProcess(4);
    QueryPerformanceCounter(&end);
    printf("NtQueryInformationFile(position) %.1f ns/call\n",(double)(end.QuadPart-start.QuadPart)*1e9/freq.QuadPart/20000);
    CloseHandle(file);
}


#define CHECK(expr) do { if (!(expr)) { printf("CHECK failure line=%u error=%lu\n",__LINE__,GetLastError()); return 20; } } while(0)
static volatile LONG callback_count;
static VOID CALLBACK callback(ULONG_PTR arg) { callback_count += (LONG)arg; }
static HANDLE wait_target, ready;
static DWORD WINAPI worker(void *unused) {
    SetEvent(ready);
    return WaitForSingleObject(wait_target, 1000) == WAIT_OBJECT_0 ? 0 : 1;
}
static int checks(void) {
    HANDLE event=CreateEventW(NULL,FALSE,FALSE,NULL),sem=CreateSemaphoreW(NULL,1,3,NULL),mutex=CreateMutexW(NULL,TRUE,NULL);
    HANDLE both[2],thread,file,duplicate; DWORD code,read; LONG previous,high; LARGE_INTEGER distance,position,size;
    char bytes[16],initial[]="0123456789abcdef";
    CHECK(event && sem && mutex);
    CHECK(WaitForSingleObject(event,0)==WAIT_TIMEOUT);
    CHECK(SetEvent(event)); CHECK(WaitForSingleObject(event,0)==WAIT_OBJECT_0);
    CHECK(WaitForSingleObject(event,0)==WAIT_TIMEOUT);
    CHECK(SetEvent(event)); CHECK(ResetEvent(event)); CHECK(WaitForSingleObject(event,0)==WAIT_TIMEOUT);
    CHECK(DuplicateHandle(GetCurrentProcess(),sem,GetCurrentProcess(),&duplicate,0,FALSE,DUPLICATE_SAME_ACCESS));
    both[0]=sem; both[1]=duplicate;
    CHECK(WaitForMultipleObjects(2,both,TRUE,0)==WAIT_FAILED && GetLastError()==ERROR_INVALID_PARAMETER);
    CloseHandle(duplicate);
    both[0]=event; both[1]=sem;
    CHECK(WaitForMultipleObjects(2,both,FALSE,0)==WAIT_OBJECT_0+1);
    CHECK(WaitForSingleObject(sem,0)==WAIT_TIMEOUT);
    CHECK(ReleaseSemaphore(sem,2,&previous) && previous==0);
    CHECK(SetEvent(event)); CHECK(WaitForMultipleObjectsEx(2,both,TRUE,0,FALSE)==WAIT_OBJECT_0);
    CHECK(WaitForSingleObjectEx(sem,0,FALSE)==WAIT_OBJECT_0);
    CHECK(ReleaseMutex(mutex)); CHECK(WaitForSingleObject(mutex,0)==WAIT_OBJECT_0);
    CHECK(ReleaseMutex(mutex));
    SetLastError(0x12345678); CHECK(!ReleaseSemaphore(sem,0,NULL) && GetLastError()==ERROR_INVALID_PARAMETER);
    CHECK(!ReleaseMutex(mutex) && GetLastError()==ERROR_NOT_OWNER);
    CHECK(WaitForSingleObject((HANDLE)0x12345678,0)==WAIT_FAILED && GetLastError()==ERROR_INVALID_HANDLE);
    CHECK(QueueUserAPC(callback,GetCurrentThread(),3));
    CHECK(WaitForSingleObjectEx(event,0,TRUE)==WAIT_IO_COMPLETION && callback_count==3);
    ready=CreateEventW(NULL,FALSE,FALSE,NULL); wait_target=event;
    thread=CreateThread(NULL,0,worker,NULL,0,NULL); CHECK(thread!=NULL);
    CHECK(WaitForSingleObject(ready,1000)==WAIT_OBJECT_0); Sleep(2);
    CHECK(SetEvent(event)); CHECK(WaitForSingleObject(thread,1000)==WAIT_OBJECT_0);
    CHECK(GetExitCodeThread(thread,&code) && code==0);
    CloseHandle(thread); CloseHandle(ready);
    wait_target=sem; ready=CreateEventW(NULL,FALSE,FALSE,NULL);
    thread=CreateThread(NULL,0,worker,NULL,0,NULL); CHECK(thread!=NULL);
    CHECK(WaitForSingleObject(ready,1000)==WAIT_OBJECT_0); Sleep(2);
    CHECK(ReleaseSemaphore(sem,1,NULL)); CHECK(WaitForSingleObject(thread,1000)==WAIT_OBJECT_0);
    CHECK(GetExitCodeThread(thread,&code) && code==0);
    CloseHandle(thread); CloseHandle(ready);
    CloseHandle(event); CloseHandle(sem); CloseHandle(mutex);
    printf("CHECK objects: events, semaphores, mutexes, zero waits, APCs, wakeups\n");
    file=CreateFileW(L"service-file.tmp",GENERIC_READ|GENERIC_WRITE,FILE_SHARE_READ|FILE_SHARE_WRITE,NULL,CREATE_ALWAYS,0,NULL);
    CHECK(file!=INVALID_HANDLE_VALUE);
    CHECK(WriteFile(file,initial,16,&read,NULL) && read==16);
    CHECK(GetFileSize(file,NULL)==16); CHECK(GetFileSizeEx(file,&size) && size.QuadPart==16);
    CHECK(SetFilePointer(file,3,NULL,FILE_BEGIN)==3);
    CHECK(ReadFile(file,bytes,4,&read,NULL) && read==4 && !memcmp(bytes,"3456",4));
    CHECK(SetFilePointer(file,0,NULL,FILE_CURRENT)==7);
    CHECK(DuplicateHandle(GetCurrentProcess(),file,GetCurrentProcess(),&duplicate,0,FALSE,DUPLICATE_SAME_ACCESS));
    distance.QuadPart=2; CHECK(SetFilePointerEx(duplicate,distance,&position,FILE_CURRENT) && position.QuadPart==9);
    CHECK(SetFilePointer(file,0,NULL,FILE_CURRENT)==9);
    CHECK(ReadFile(file,bytes,2,&read,NULL) && read==2 && !memcmp(bytes,"9a",2));
    CHECK(SetFilePointer(duplicate,0,NULL,FILE_CURRENT)==11);
    distance.QuadPart=-3; CHECK(SetFilePointerEx(file,distance,&position,FILE_END) && position.QuadPart==13);
    CHECK(!SetFilePointerEx(file,distance,&position,FILE_BEGIN) && GetLastError()==ERROR_NEGATIVE_SEEK);
    high=0; CHECK(SetFilePointer(file,0xffffffff,&high,FILE_BEGIN)==0xffffffff && high==0 && GetLastError()==0);
    distance.QuadPart=0; CHECK(SetFilePointerEx(file,distance,&position,FILE_BEGIN));
    CloseHandle(duplicate); CloseHandle(file); DeleteFileW(L"service-file.tmp");
    {
        HANDLE pipe[2], nonfile[3];
        CHECK(CreatePipe(&pipe[0],&pipe[1],NULL,4096));
        CHECK(CreateDirectoryW(L"service-dir.tmp",NULL));
        nonfile[0]=CreateFileW(L"service-dir.tmp",GENERIC_READ,FILE_SHARE_READ|FILE_SHARE_WRITE|FILE_SHARE_DELETE,NULL,OPEN_EXISTING,FILE_FLAG_BACKUP_SEMANTICS,NULL);
        nonfile[1]=pipe[0]; nonfile[2]=CreateFileW(L"NUL",GENERIC_READ|GENERIC_WRITE,FILE_SHARE_READ|FILE_SHARE_WRITE,NULL,OPEN_EXISTING,0,NULL);
        for(unsigned n=0;n<3;n++) {
            CHECK(nonfile[n]!=INVALID_HANDLE_VALUE);
            CHECK(GetFileSizeEx(nonfile[n],&size) && size.QuadPart==0);
            distance.QuadPart=1; CHECK(SetFilePointerEx(nonfile[n],distance,&position,FILE_BEGIN) && position.QuadPart==1);
            CHECK(SetFilePointer(nonfile[n],0,NULL,FILE_CURRENT)==1);
            CloseHandle(nonfile[n]);
        }
        CloseHandle(pipe[1]);
        CHECK(RemoveDirectoryW(L"service-dir.tmp"));
        distance.QuadPart=-1; CHECK(!SetFilePointerEx(INVALID_HANDLE_VALUE,distance,NULL,FILE_BEGIN) && GetLastError()==ERROR_NEGATIVE_SEEK);
    }
    printf("CHECK files: sizes, positions, reads, duplicate handles, errors\n");
    return 0;
}

static void benchmark_bodies(void) {
    LARGE_INTEGER start,end,freq; static LARGE_INTEGER value; HANDLE event=CreateEventW(NULL,TRUE,TRUE,NULL),sem=CreateSemaphoreW(NULL,0,0x7fffffff,NULL),mutex=CreateMutexW(NULL,TRUE,NULL);
    DWORD i; HANDLE pair[2]={event,event};
    HMODULE nt=GetModuleHandleA("ntdll.dll");
    BOOL (WINAPI *rtl)(LARGE_INTEGER *)=(void *)GetProcAddress(nt,"RtlQueryPerformanceCounter");
    LONG (WINAPI *ntqpc)(LARGE_INTEGER *,LARGE_INTEGER *)=(void *)GetProcAddress(nt,"NtQueryPerformanceCounter");
    HANDLE file=CreateFileW(L"service-size.tmp",GENERIC_READ|GENERIC_WRITE,FILE_SHARE_READ,NULL,CREATE_ALWAYS,0,NULL);
    LARGE_INTEGER zero; zero.QuadPart=0; QueryPerformanceFrequency(&freq);
#define BENCH(label,expression) do { for(i=0;i<1000;i++) { expression; } QueryPerformanceCounter(&start); for(i=0;i<100000;i++) { expression; } QueryPerformanceCounter(&end); printf("BENCH %s %.1f ns/call\n",label,(double)(end.QuadPart-start.QuadPart)*1e9/freq.QuadPart/100000); } while(0)
    BENCH("QueryPerformanceCounter",QueryPerformanceCounter(&value));
    BENCH("RtlQueryPerformanceCounter",rtl(&value));
    BENCH("NtQueryPerformanceCounter",ntqpc(&value,NULL));
    BENCH("SetEvent",SetEvent(event)); BENCH("ResetEvent",ResetEvent(event));
    BENCH("ReleaseSemaphore",ReleaseSemaphore(sem,1,NULL));
    /* Recursive acquisition retains ownership across releases. */
    for(i=0;i<102000;i++) WaitForSingleObject(mutex,0);
    BENCH("ReleaseMutex",ReleaseMutex(mutex));
    BENCH("WaitForSingleObject",WaitForSingleObject(event,0));
    BENCH("WaitForSingleObjectEx",WaitForSingleObjectEx(event,0,FALSE));
    BENCH("WaitForMultipleObjects",WaitForMultipleObjects(2,pair,FALSE,0));
    BENCH("WaitForMultipleObjectsEx",WaitForMultipleObjectsEx(2,pair,FALSE,0,FALSE));
    BENCH("GetFileSize",GetFileSize(file,NULL)); BENCH("GetFileSizeEx",GetFileSizeEx(file,&value));
    BENCH("SetFilePointer",SetFilePointer(file,0,NULL,FILE_CURRENT));
    BENCH("SetFilePointerEx",SetFilePointerEx(file,zero,&value,FILE_CURRENT));
    CloseHandle(file); DeleteFileW(L"service-size.tmp"); CloseHandle(event); CloseHandle(sem); CloseHandle(mutex);
}
static void audit_guest_time_queries(void) {
    LONG (WINAPI *alert)(void)=(void *)GetProcAddress(GetModuleHandleA("ntdll.dll"),"NtTestAlert");
    BOOL (WINAPI *rtl)(LARGE_INTEGER *)=(void *)GetProcAddress(GetModuleHandleA("ntdll.dll"),"RtlQueryPerformanceCounter");
    LONG (WINAPI *nt)(LARGE_INTEGER *,LARGE_INTEGER *)=(void *)GetProcAddress(GetModuleHandleA("ntdll.dll"),"NtQueryPerformanceCounter");
    static LARGE_INTEGER counter;
    LARGE_INTEGER frequency;
    SetLastError(0x5a17e20); alert(); QueryPerformanceFrequency(&frequency); SetLastError(0x5a17e21); alert();
    SetLastError(0x5a17e22); alert(); GetTickCount(); SetLastError(0x5a17e23); alert();
    SetLastError(0x5a17e24); alert(); GetTickCount64(); SetLastError(0x5a17e25); alert();
    /* Warm both call sites and output storage before auditing the boundary. */
    for(unsigned n=0;n<1000;n++) { QueryPerformanceCounter(&counter); rtl(&counter); nt(&counter,NULL); }
    SetLastError(0x5a17e26); alert(); for(unsigned n=0;n<1000;n++) QueryPerformanceCounter(&counter); SetLastError(0x5a17e27); alert();
    SetLastError(0x5a17e28); alert(); for(unsigned n=0;n<1000;n++) rtl(&counter); SetLastError(0x5a17e29); alert();
    SetLastError(0x5a17e2a); alert(); for(unsigned n=0;n<1000;n++) nt(&counter,NULL); SetLastError(0x5a17e2b); alert();
}
static void warm_direct_body(void) {
    static char bytes[32]; unsigned int i;
    void *(__cdecl *fill)(void *,int,size_t)=(void *)GetProcAddress(GetModuleHandleA("ntdll.dll"),"memset");
    for(i=0;i<16;i++) fill(bytes,0,sizeof(bytes));
}
int main(void) {
    LONG (WINAPI *test_alert)(void)=(void *)GetProcAddress(GetModuleHandleA("ntdll.dll"),"NtTestAlert");
    SetLastError(0x5a17e13); test_alert();
    audit_guest_time_queries();
    int status=checks(); if(status) return status;
    warm_direct_body(); benchmark_system_calls(); benchmark_bodies(); return 0;
}
