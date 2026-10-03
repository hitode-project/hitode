/* Authored multimedia timer fixture; no window/device/game/network. */
#include "win32_min.h"
API UINT WINAPI timeGetDevCaps(void *,UINT);
API DWORD WINAPI timeGetTime(void);
API UINT WINAPI timeSetEvent(UINT,UINT,void *,DWORD,UINT);
API UINT WINAPI timeKillEvent(UINT);
API UINT WINAPI timeBeginPeriod(UINT);
API UINT WINAPI timeEndPeriod(UINT);
API BOOL WINAPI ResetEvent(HANDLE);
API DWORD WINAPI GetCurrentThreadId(void);
API HANDLE WINAPI CreateThread(void *,DWORD,THREADPROC,void *,DWORD,DWORD *);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static DWORD report[256],size;
static HANDLE done,started,release,kill_started;
static volatile DWORD expected_id,worker_tid,same_worker=1,args_valid=1,ended,called,self_kill;
static void emit(DWORD value) { report[size++]=value; }
static void WINAPI callback(UINT id,UINT message,DWORD user,DWORD one,DWORD two) {
    DWORD tid=GetCurrentThreadId();
    if(!worker_tid) worker_tid=tid; else same_worker &= worker_tid==tid;
    args_valid &= id==expected_id && message==0 && one==0 && two==0 && (user&0xffffff00u)==0xabcdef00u;
    called++;
    if((user&255)==2 || (user&255)==4) self_kill=timeKillEvent(id);
    if((user&255)==3) { SetEvent(started); WaitForSingleObject(release,INFINITE); }
    ended=1; SetEvent(done);
}
static DWORD WINAPI releaser(void *unused) {
    (void)unused; WaitForSingleObject(kill_started,INFINITE); Sleep(20); SetEvent(release); return 0;
}
static void reset(void) { ResetEvent(done); ResetEvent(started); ResetEvent(release); ResetEvent(kill_started); ended=0; called=0; }
void mainCRTStartup(void) {
    DWORD written,i,main_tid=GetCurrentThreadId(); HANDLE f,h; DWORD caps[3]; UINT id;
    UINT sizes[]={0,1,4,7,8,9,12};
    for(i=0;i<7;++i) { caps[0]=caps[1]=caps[2]=0xcccccccc; emit(timeGetDevCaps(caps,sizes[i])); emit(caps[0]);emit(caps[1]);emit(caps[2]); }
    emit(timeGetDevCaps(NULL,8)); emit(timeKillEvent(0xfedcba98));
    emit(timeSetEvent(0,0,NULL,0,0)!=0); emit(timeSetEvent(1000001,0,NULL,0,0)!=0); emit(timeSetEvent(50,0,NULL,0,2)!=0);
    id=timeSetEvent(1000000,0,NULL,0,0x30); emit(id!=0);emit(timeKillEvent(id));emit(timeKillEvent(id));
    emit(timeEndPeriod(0)); emit(timeEndPeriod(1));emit(timeEndPeriod(16));emit(timeEndPeriod(1000001));
    done=CreateEventA(NULL,1,0,NULL); started=CreateEventA(NULL,1,0,NULL); release=CreateEventA(NULL,1,0,NULL); kill_started=CreateEventA(NULL,1,0,NULL);
    reset(); expected_id=timeSetEvent(20,0,callback,0xabcdef01,0); emit(expected_id!=0);
    emit(WaitForSingleObject(done,1000)); emit(called==1); Sleep(20); emit(timeKillEvent(expected_id));
    reset(); expected_id=timeSetEvent(20,0,callback,0xabcdef02,1); emit(expected_id!=0);
    emit(WaitForSingleObject(done,1000)); Sleep(20); emit(called==1); emit(self_kill);emit(timeKillEvent(expected_id));
    reset(); expected_id=timeSetEvent(20,0,callback,0xabcdef04,0x101); emit(expected_id!=0);
    emit(WaitForSingleObject(done,1000)); Sleep(20); emit(called==1); emit(self_kill);emit(timeKillEvent(expected_id));
    reset(); expected_id=timeSetEvent(20,0,callback,0xabcdef03,1); emit(expected_id!=0);
    emit(WaitForSingleObject(started,1000)); emit(timeKillEvent(expected_id)); emit(ended);
    SetEvent(release);emit(WaitForSingleObject(done,1000));emit(ended);
    reset(); expected_id=timeSetEvent(20,0,callback,0xabcdef03,0x101); emit(expected_id!=0);
    emit(WaitForSingleObject(started,1000)); h=CreateThread(NULL,0,releaser,NULL,0,NULL); SetEvent(kill_started);
    emit(timeKillEvent(expected_id)); emit(ended); emit(WaitForSingleObject(h,1000));CloseHandle(h);
    reset(); id=timeSetEvent(20,0,(void *)done,0,0x10); emit(id!=0);emit(WaitForSingleObject(done,1000));emit(WaitForSingleObject(done,0));
    reset(); id=timeSetEvent(20,0,(void *)done,0,0x20); emit(id!=0);emit(WaitForSingleObject(done,1000));emit(WaitForSingleObject(done,0));
    emit(worker_tid!=main_tid);emit(same_worker);emit(args_valid);
    CloseHandle(done);CloseHandle(started);CloseHandle(release);CloseHandle(kill_started);
    f=CreateFileA("winmm-report.bin",0x40000000,0,NULL,2,0,0);
    if(f==0xffffffffu||!WriteFile(f,report,size*4,&written,NULL)||written!=size*4) ExitProcess(2);
    CloseHandle(f);ExitProcess(0);
}
