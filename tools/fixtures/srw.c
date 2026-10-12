/* Authored SRW lock oracle: no inputs; srw.bin contains API result scalars.
 * Own worker threads exercise shared coexistence and real contended waits. */
#include <windows.h>
static SRWLOCK lock=SRWLOCK_INIT;
static DWORD results[256],used,worker_ok[2];
static HANDLE acquired[2],finish[2],started[2];
static void record(DWORD value){results[used++]=value;}
typedef void (WINAPI *LOCK_FN)(SRWLOCK *);
typedef BOOLEAN (WINAPI *TRY_FN)(SRWLOCK *);
static DWORD WINAPI reader(void *arg){
 int index=(int)(INT_PTR)arg;SetEvent(started[index]);AcquireSRWLockShared(&lock);SetEvent(acquired[index]);
 worker_ok[index]=WaitForSingleObject(finish[index],5000)==WAIT_OBJECT_0;
 ReleaseSRWLockShared(&lock);return 0;
}
static DWORD WINAPI writer(void *arg){
 SetEvent(started[0]);AcquireSRWLockExclusive(&lock);SetEvent(acquired[0]);
 worker_ok[0]=WaitForSingleObject(finish[0],5000)==WAIT_OBJECT_0;
 ReleaseSRWLockExclusive(&lock);return 0;
}
static void events(int count){for(int i=0;i<count;i++){acquired[i]=CreateEventA(0,TRUE,FALSE,0);finish[i]=CreateEventA(0,TRUE,FALSE,0);started[i]=CreateEventA(0,TRUE,FALSE,0);}}
static void close_events(int count){for(int i=0;i<count;i++){CloseHandle(acquired[i]);CloseHandle(finish[i]);CloseHandle(started[i]);}}
void mainCRTStartup(void){
 HMODULE ntdll=GetModuleHandleA("ntdll.dll");
 LOCK_FN initialize=(LOCK_FN)GetProcAddress(ntdll,"RtlInitializeSRWLock");
 LOCK_FN exclusive=(LOCK_FN)GetProcAddress(ntdll,"RtlAcquireSRWLockExclusive");
 LOCK_FN shared=(LOCK_FN)GetProcAddress(ntdll,"RtlAcquireSRWLockShared");
 TRY_FN try_exclusive=(TRY_FN)GetProcAddress(ntdll,"RtlTryAcquireSRWLockExclusive");
 TRY_FN try_shared=(TRY_FN)GetProcAddress(ntdll,"RtlTryAcquireSRWLockShared");
 LOCK_FN release_exclusive=(LOCK_FN)GetProcAddress(ntdll,"RtlReleaseSRWLockExclusive");
 LOCK_FN release_shared=(LOCK_FN)GetProcAddress(ntdll,"RtlReleaseSRWLockShared");
 record(initialize&&exclusive&&shared&&try_exclusive&&try_shared&&release_exclusive&&release_shared);
 initialize(&lock);record(lock.Ptr==0);record(try_exclusive(&lock));record(TryAcquireSRWLockExclusive(&lock));record(try_shared(&lock));ReleaseSRWLockExclusive(&lock);
 exclusive(&lock);record(TryAcquireSRWLockShared(&lock));release_exclusive(&lock);
 shared(&lock);record(TryAcquireSRWLockExclusive(&lock));release_shared(&lock);record(TryAcquireSRWLockShared(&lock));ReleaseSRWLockShared(&lock);record(lock.Ptr==0);
 events(2);AcquireSRWLockExclusive(&lock);
 HANDLE threads[2];for(int i=0;i<2;i++){threads[i]=CreateThread(0,0,reader,(void *)(INT_PTR)i,0,0);record(threads[i]!=0);record(WaitForSingleObject(started[i],5000));record(WaitForSingleObject(acquired[i],10));}
 ReleaseSRWLockExclusive(&lock);
 for(int i=0;i<2;i++)record(WaitForSingleObject(acquired[i],5000));
 record(TryAcquireSRWLockExclusive(&lock));record(try_shared(&lock));release_shared(&lock);
 for(int i=0;i<2;i++)SetEvent(finish[i]);
 for(int i=0;i<2;i++){record(WaitForSingleObject(threads[i],5000));record(worker_ok[i]);CloseHandle(threads[i]);}close_events(2);
 for(int mode=0;mode<2;mode++){
  events(1);if(mode)AcquireSRWLockShared(&lock);else AcquireSRWLockExclusive(&lock);
  HANDLE thread=CreateThread(0,0,writer,0,0,0);record(thread!=0);record(WaitForSingleObject(started[0],5000));record(WaitForSingleObject(acquired[0],10));
  if(mode)ReleaseSRWLockShared(&lock);else ReleaseSRWLockExclusive(&lock);
  record(WaitForSingleObject(acquired[0],5000));record(TryAcquireSRWLockExclusive(&lock));record(TryAcquireSRWLockShared(&lock));SetEvent(finish[0]);record(WaitForSingleObject(thread,5000));record(worker_ok[0]);CloseHandle(thread);close_events(1);
 }
 InitializeSRWLock(&lock);record(lock.Ptr==0);record(TryAcquireSRWLockExclusive(&lock));ReleaseSRWLockExclusive(&lock);
 HANDLE file=CreateFileA("srw.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,results,used*4,&written,0)||written!=used*4)ExitProcess(250);
 CloseHandle(file);ExitProcess(0);
}
