/* Authored condition-variable oracle. No input; condition.bin contains scalars.
 * Own threads validate wake-one/all, nonpersistent wakes, timeout and relocking. */
#include <windows.h>
static CONDITION_VARIABLE condition=CONDITION_VARIABLE_INIT;
static SRWLOCK lock=SRWLOCK_INIT;
static CRITICAL_SECTION section;
static HANDLE ready,completed;
static volatile LONG entered,tokens,returned;
static int mode;
static DWORD results[256],used,worker_ok[2];
typedef LONG (WINAPI *SLEEP_CS)(CONDITION_VARIABLE *,CRITICAL_SECTION *,LARGE_INTEGER *);
typedef LONG (WINAPI *SLEEP_SRW)(CONDITION_VARIABLE *,SRWLOCK *,LARGE_INTEGER *,ULONG);
typedef void (WINAPI *CV_FN)(CONDITION_VARIABLE *);
static SLEEP_CS native_cs;
static SLEEP_SRW native_srw;
static CV_FN native_wake_all,native_initialize;
static void record(DWORD value){results[used++]=value;}
static void acquire(void){if(mode==0)EnterCriticalSection(&section);else if(mode==1)AcquireSRWLockExclusive(&lock);else AcquireSRWLockShared(&lock);}
static void release(void){if(mode==0)LeaveCriticalSection(&section);else if(mode==1)ReleaseSRWLockExclusive(&lock);else ReleaseSRWLockShared(&lock);}
static BOOL sleeping(int native,DWORD milliseconds){
 LARGE_INTEGER interval;interval.QuadPart=-(LONGLONG)milliseconds*10000;
 if(native)return (mode==0?native_cs(&condition,&section,&interval):native_srw(&condition,&lock,&interval,mode==2?1:0))==0;
 return mode==0?SleepConditionVariableCS(&condition,&section,milliseconds):SleepConditionVariableSRW(&condition,&lock,milliseconds,mode==2?1:0);
}
static DWORD WINAPI worker(void *arg){
 int index=(int)(INT_PTR)arg;acquire();if(InterlockedIncrement(&entered)==2)SetEvent(ready);
 worker_ok[index]=1;
 while(tokens==0)if(!sleeping(index,5000)){worker_ok[index]=0;break;}
 if(worker_ok[index]){InterlockedDecrement(&tokens);InterlockedIncrement(&returned);SetEvent(completed);}
 release();return 0;
}
void mainCRTStartup(void){
 HMODULE ntdll=GetModuleHandleA("ntdll.dll");
 native_cs=(SLEEP_CS)GetProcAddress(ntdll,"RtlSleepConditionVariableCS");native_srw=(SLEEP_SRW)GetProcAddress(ntdll,"RtlSleepConditionVariableSRW");
 native_wake_all=(CV_FN)GetProcAddress(ntdll,"RtlWakeAllConditionVariable");native_initialize=(CV_FN)GetProcAddress(ntdll,"RtlInitializeConditionVariable");
 record(native_cs&&native_srw&&native_wake_all&&native_initialize);InitializeCriticalSection(&section);
 for(mode=0;mode<3;mode++){
  InitializeConditionVariable(&condition);InitializeSRWLock(&lock);record(condition.Ptr==0);
  WakeConditionVariable(&condition);native_wake_all(&condition);acquire();SetLastError(777);record(sleeping(0,5));record(GetLastError());
  if(mode==0){record(section.RecursionCount==1);record((DWORD)(UINT_PTR)section.OwningThread==GetCurrentThreadId());}
  else {record(TryAcquireSRWLockExclusive(&lock));record(mode==1?TryAcquireSRWLockShared(&lock):0);}release();
  acquire();LARGE_INTEGER interval;interval.QuadPart=-50000;SetLastError(777);
  record(mode==0?native_cs(&condition,&section,&interval):native_srw(&condition,&lock,&interval,mode==2?1:0));record(GetLastError());release();
  native_initialize(&condition);entered=0;returned=0;tokens=0;ready=CreateEventA(0,TRUE,FALSE,0);completed=CreateEventA(0,FALSE,FALSE,0);
  HANDLE threads[2];for(int i=0;i<2;i++){threads[i]=CreateThread(0,0,worker,(void *)(INT_PTR)i,0,0);record(threads[i]!=0);}
  record(WaitForSingleObject(ready,5000));
  /* Exclusive ownership also waits until shared-mode sleepers released their locks. */
  if(mode==0)EnterCriticalSection(&section);else AcquireSRWLockExclusive(&lock);
  tokens=1;WakeConditionVariable(&condition);if(mode==0)LeaveCriticalSection(&section);else ReleaseSRWLockExclusive(&lock);
  record(WaitForSingleObject(completed,5000));record(returned==1);
  if(mode==0)EnterCriticalSection(&section);else AcquireSRWLockExclusive(&lock);
  tokens=1;native_wake_all(&condition);if(mode==0)LeaveCriticalSection(&section);else ReleaseSRWLockExclusive(&lock);
  for(int i=0;i<2;i++){record(WaitForSingleObject(threads[i],5000));record(worker_ok[i]);CloseHandle(threads[i]);}record(returned==2);CloseHandle(ready);CloseHandle(completed);
 }
 DeleteCriticalSection(&section);
 HANDLE file=CreateFileA("condition.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,results,used*4,&written,0)||written!=used*4)ExitProcess(250);
 CloseHandle(file);ExitProcess(0);
}
