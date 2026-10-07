/* Authored registered-wait oracle; inputs are events/timeouts, outputs are scalar checks. */
#include <windows.h>
typedef struct {HANDLE entered,release; volatile LONG count; BOOL timed;} STATE;
static DWORD values[64];
static STATE state;
static PTP_TIMER expected_timer;
static BOOL timer_identity;
static PTP_WAIT expected_wait;
static BOOL wait_identity;
static TP_WAIT_RESULT wait_result;
static VOID CALLBACK wait_callback(PTP_CALLBACK_INSTANCE instance,PVOID context,PTP_WAIT wait,TP_WAIT_RESULT result) {
 wait_identity=instance!=NULL&&context==&state&&wait==expected_wait;
 wait_result=result;InterlockedIncrement(&state.count);SetEvent(state.entered);
}
static VOID CALLBACK timer_callback(PTP_CALLBACK_INSTANCE instance,PVOID context,PTP_TIMER timer) {
 timer_identity=instance!=NULL&&context==&state&&timer==expected_timer;
 InterlockedIncrement(&state.count);SetEvent(state.entered);
}
static VOID CALLBACK callback(PVOID context,BOOLEAN timeout) {
 STATE *s=context; InterlockedIncrement(&s->count);s->timed=timeout;
 SetEvent(s->entered); if(s->release)WaitForSingleObject(s->release,INFINITE);
}
void mainCRTStartup(void) {
 HANDLE event,wait,complete,file; DWORD n=0,written;LARGE_INTEGER due;LONG before;
 state.entered=CreateEventW(NULL,FALSE,FALSE,NULL);
 event=CreateEventW(NULL,FALSE,FALSE,NULL);
 values[n++]=RegisterWaitForSingleObject(&wait,event,callback,&state,INFINITE,0)!=0;
 SetEvent(event);values[n++]=WaitForSingleObject(state.entered,5000)==0;
 values[n++]=state.count==1&&!state.timed;
 values[n++]=UnregisterWaitEx(wait,INVALID_HANDLE_VALUE)!=0;
 state.count=0;state.timed=FALSE;
 values[n++]=RegisterWaitForSingleObject(&wait,event,callback,&state,5,WT_EXECUTEONLYONCE)!=0;
 values[n++]=WaitForSingleObject(state.entered,5000)==0;
 values[n++]=UnregisterWaitEx(wait,INVALID_HANDLE_VALUE)!=0;
 values[n++]=state.count==1&&state.timed;
 state.count=0;state.timed=FALSE;
 values[n++]=RegisterWaitForSingleObject(&wait,event,callback,&state,INFINITE,0)!=0;
 values[n++]=UnregisterWaitEx(wait,INVALID_HANDLE_VALUE)!=0;
 SetEvent(event);Sleep(10);values[n++]=state.count==0;ResetEvent(event);
 complete=CreateEventW(NULL,TRUE,FALSE,NULL);
 state.release=CreateEventW(NULL,TRUE,FALSE,NULL);
 values[n++]=RegisterWaitForSingleObject(&wait,event,callback,&state,INFINITE,0)!=0;
 SetEvent(event);values[n++]=WaitForSingleObject(state.entered,5000)==0;
 UnregisterWaitEx(wait,complete);
 values[n++]=WaitForSingleObject(complete,0)==WAIT_TIMEOUT;
 SetEvent(state.release);values[n++]=WaitForSingleObject(complete,5000)==0;
 values[n++]=state.count==1&&!state.timed;
 CloseHandle(state.release);CloseHandle(complete);
 state.count=0;state.release=NULL;
 expected_timer=CreateThreadpoolTimer(timer_callback,&state,NULL);
 values[n++]=expected_timer!=NULL;
 due.QuadPart=-100000;
 SetThreadpoolTimer(expected_timer,(FILETIME*)&due,0,0);
 values[n++]=WaitForSingleObject(state.entered,5000)==0;
 WaitForThreadpoolTimerCallbacks(expected_timer,FALSE);
 values[n++]=state.count==1&&timer_identity;
 due.QuadPart=-200000;
 SetThreadpoolTimer(expected_timer,(FILETIME*)&due,0,0);
 WaitForThreadpoolTimerCallbacks(expected_timer,TRUE);
 values[n++]=WaitForSingleObject(state.entered,50)==WAIT_TIMEOUT;
 WaitForThreadpoolTimerCallbacks(expected_timer,FALSE);
 values[n++]=state.count==1;
 due.QuadPart=-100000;
 SetThreadpoolTimer(expected_timer,(FILETIME*)&due,5,0);
 values[n++]=WaitForSingleObject(state.entered,5000)==0;
 SetThreadpoolTimer(expected_timer,NULL,0,0);
 WaitForThreadpoolTimerCallbacks(expected_timer,TRUE);
 before=state.count;Sleep(20);values[n++]=state.count==before;
 CloseThreadpoolTimer(expected_timer);
 state.count=0;ResetEvent(state.entered);
 expected_wait=CreateThreadpoolWait(wait_callback,&state,NULL);
 values[n++]=expected_wait!=NULL;
 SetThreadpoolWait(expected_wait,event,NULL);SetEvent(event);
 values[n++]=WaitForSingleObject(state.entered,5000)==0;
 WaitForThreadpoolWaitCallbacks(expected_wait,FALSE);
 values[n++]=state.count==1&&wait_identity&&wait_result==WAIT_OBJECT_0;
 SetEvent(event);Sleep(10);values[n++]=state.count==1;ResetEvent(event);
 SetThreadpoolWait(expected_wait,event,(FILETIME*)&due);
 values[n++]=WaitForSingleObject(state.entered,5000)==0;
 WaitForThreadpoolWaitCallbacks(expected_wait,FALSE);
 values[n++]=state.count==2&&wait_result==WAIT_TIMEOUT;
 SetThreadpoolWait(expected_wait,NULL,NULL);
 WaitForThreadpoolWaitCallbacks(expected_wait,TRUE);CloseThreadpoolWait(expected_wait);
 CloseHandle(state.entered);CloseHandle(event);
 file=CreateFileW(L"waits.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,n*4,&written,NULL)||written!=n*4)ExitProcess(2);
 CloseHandle(file);ExitProcess(0);
}
