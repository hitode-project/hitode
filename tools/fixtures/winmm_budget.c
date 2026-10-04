/* Authored guest-only resource/fairness regression. No physical device. */
#include "win32_min.h"
API UINT WINAPI timeSetEvent(UINT,UINT,void *,DWORD,UINT);
API UINT WINAPI timeKillEvent(UINT);
API UINT WINAPI timeBeginPeriod(UINT);
API UINT WINAPI timeEndPeriod(UINT);
API DWORD WINAPI GetCurrentThreadId(void);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
typedef DWORD (WINAPI *TESTCALL)(DWORD);
static DWORD report[32],count,ids[256];
static volatile DWORD consumer_seen,callbacks,worker_tid,same_worker=1;
static HANDLE audio_ready,done;
static TESTCALL signal_audio,advance_guest;
static void emit(DWORD value) { report[count++]=value; }
static DWORD WINAPI audio_consumer(void *unused) {
    (void)unused; WaitForSingleObject(audio_ready,INFINITE); consumer_seen=1; return 0;
}
static void WINAPI callback(UINT id,UINT message,DWORD user,DWORD one,DWORD two) {
    DWORD tid=GetCurrentThreadId(); (void)id;(void)message;(void)user;(void)one;(void)two;
    if(!worker_tid) worker_tid=tid; else same_worker &= worker_tid==tid;
    if(callbacks++==0) signal_audio(audio_ready);
    else { emit(consumer_seen); SetEvent(done); }
}
void mainCRTStartup(void) {
    DWORD i,pass,written; HANDLE file,thread;
    /* The test registers this private module before guest execution. Its EAT
       must be created after registration, unlike imported winmm.dll. */
    HMODULE mm=LoadLibraryA("adv_winmm_test.dll");
    if(!mm) ExitProcess(3);
    signal_audio=(TESTCALL)GetProcAddress(mm,"__test_audio_ready");
    advance_guest=(TESTCALL)GetProcAddress(mm,"__test_advance_guest");
    if(!signal_audio||!advance_guest) ExitProcess(4);
    emit(timeEndPeriod(1));emit(timeBeginPeriod(1));emit(timeBeginPeriod(1));
    emit(timeEndPeriod(1));emit(timeEndPeriod(1));emit(timeEndPeriod(1));
    emit(timeBeginPeriod(0));emit(timeEndPeriod(0));
    emit(timeBeginPeriod(0xffffffffu));emit(timeEndPeriod(0xffffffffu));
    for(pass=0;pass<2;++pass) {
        DWORD accepted=1,canceled=1;
        for(i=0;i<256;++i) { ids[i]=timeSetEvent(1000000,0,NULL,0,0);accepted &= ids[i]!=0; }
        emit(accepted);emit(timeSetEvent(1000000,0,NULL,0,0)==0);
        for(i=0;i<256;++i) canceled &= timeKillEvent(ids[i])==0;
        emit(canceled);
    }
    audio_ready=CreateEventA(NULL,0,0,NULL);done=CreateEventA(NULL,1,0,NULL);
    thread=CreateThread(NULL,0,audio_consumer,NULL,0,NULL);
    emit(timeSetEvent(10,0,callback,0,0)!=0);emit(timeSetEvent(10,0,callback,0,0)!=0);
    /* Both callbacks are already due when the worker resumes. */
    advance_guest(100000);
    emit(WaitForSingleObject(done,1000));emit(WaitForSingleObject(thread,1000));
    emit(callbacks==2);emit(same_worker);emit(worker_tid!=GetCurrentThreadId());
    CloseHandle(thread);CloseHandle(done);CloseHandle(audio_ready);
    file=CreateFileA("winmm-budget.bin",0x40000000,0,NULL,2,0,0);
    if(file==0xffffffffu||!WriteFile(file,report,count*4,&written,NULL)||written!=count*4) ExitProcess(2);
    CloseHandle(file);ExitProcess(0);
}
