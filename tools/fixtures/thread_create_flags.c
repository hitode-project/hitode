/* Synthetic x86 CreateThread/CreateRemoteThread flag oracle.
 * Build with build-module-lifecycle.ps1; no inputs. thread-create-flags.bin
 * records only API results, completion and handle-publication booleans. */
#include "win32_min.h"
API HANDLE WINAPI CreateRemoteThread(HANDLE,void *,DWORD,THREADPROC,void *,DWORD,DWORD *);
API DWORD WINAPI ResumeThread(HANDLE);
API BOOL WINAPI GetExitCodeThread(HANDLE,DWORD *);
API DWORD WINAPI GetLastError(void);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static const DWORD flags[]={0,1,2,3,4,5,8,16,0x10000,0x20000,0xffffffff};
static DWORD results[256],count;
static DWORD WINAPI worker(void *parameter){return (DWORD)parameter;}
void mainCRTStartup(void) {
    DWORD remote,i,tid,code,written;HANDLE thread,file;
    for(remote=0;remote<2;remote++)for(i=0;i<sizeof(flags)/sizeof(flags[0]);i++){
        tid=0;code=0;
        thread=remote?CreateRemoteThread(0xffffffff,0,65536,worker,(void *)42,flags[i],&tid)
                     :CreateThread(0,65536,worker,(void *)42,flags[i],&tid);
        results[count++]=thread!=0;
        if(!thread){results[count++]=GetLastError();continue;}
        results[count++]=tid!=0;
        if(flags[i]&4){results[count++]=WaitForSingleObject(thread,0);results[count++]=ResumeThread(thread);}
        results[count++]=WaitForSingleObject(thread,5000);
        results[count++]=GetExitCodeThread(thread,&code)!=0;
        results[count++]=code;
        results[count++]=CloseHandle(thread)!=0;
    }
    file=CreateFileA("thread-create-flags.bin",0x40000000,0,0,2,0,0);
    if(file==0xffffffff||!WriteFile(file,results,count*4,&written,0)||written!=count*4)ExitProcess(250);
    CloseHandle(file);ExitProcess(0);
}
