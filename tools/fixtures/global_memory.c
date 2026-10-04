#define WIN32_LEAN_AND_MEAN
#include <windows.h>
static DWORD words[2048],used;
static void record(DWORD x){if(used>=2048)ExitProcess(91);words[used++]=x;}
typedef struct {
    HANDLE(WINAPI *alloc)(UINT,SIZE_T);
    LPVOID(WINAPI *lock)(HANDLE);
    BOOL(WINAPI *unlock)(HANDLE);
    UINT(WINAPI *flags)(HANDLE);
    SIZE_T(WINAPI *size)(HANDLE);
    HANDLE(WINAPI *handle)(LPCVOID);
    HANDLE(WINAPI *free)(HANDLE);
    HANDLE(WINAPI *resize)(HANDLE,SIZE_T,UINT);
} API;
void mainCRTStartup(void){
    API api[2]={{GlobalAlloc,GlobalLock,GlobalUnlock,GlobalFlags,GlobalSize,GlobalHandle,GlobalFree,GlobalReAlloc},{LocalAlloc,LocalLock,LocalUnlock,LocalFlags,LocalSize,LocalHandle,LocalFree,LocalReAlloc}};
    const DWORD sizes[]={0,1,13,64};int family,moving,size,mix;DWORD wrote;HANDLE file;
    for(family=0;family<2;family++)for(moving=0;moving<2;moving++)for(size=0;size<4;size++){
        HANDLE h=api[family].alloc((moving?2:0)|0x40,sizes[size]);LPVOID p,q;
        record(h!=NULL);record(api[family].size(h)>=sizes[size]);record(api[family].flags(h));
        SetLastError(0xcccc);p=api[family].lock(h);record(p!=NULL);record(p==h);record(p?0:GetLastError());record(api[family].flags(h));
        q=api[family].lock(h);record(q==p);record(api[family].flags(h));
        if(p){record(api[family].handle(p)==h);record(((BYTE*)p)[0]==0);}
        else{record(1);record(1);}
        SetLastError(0xcccc);record(api[family].unlock(h));record(GetLastError());record(api[family].flags(h));
        SetLastError(0xcccc);record(api[family].unlock(h));record(GetLastError());record(api[family].flags(h));
        SetLastError(0xcccc);record(api[family].unlock(h));record(GetLastError());
        record(api[family].free(h)==NULL);
    }
    /* Compatibility uses only our own allocations; no OS clipboard/input setting. */
    for(family=0;family<2;family++)for(moving=0;moving<2;moving++)for(mix=0;mix<2;mix++){
        HANDLE h=api[family].alloc((moving?2:0)|0x40,32);LPVOID p=api[1-family].lock(h);
        record(h!=NULL);record(p!=NULL);record(api[1-family].size(h)>=32);record(api[1-family].flags(h));
        if(mix){record(api[1-family].free(h)==NULL);}else{api[1-family].unlock(h);record(api[family].free(h)==NULL);}
    }
    for(family=0;family<2;family++)for(moving=0;moving<2;moving++){
        HANDLE h=api[family].alloc((moving?2:0)|0x40,13),next;BYTE *p=(BYTE*)api[family].lock(h);int i;BOOL copy=TRUE,zero=TRUE;
        for(i=0;i<13;i++)p[i]=(BYTE)(i+9);if(moving)api[family].unlock(h);
        next=api[family].resize(h,40,0x42);record(next!=NULL);if(!next)ExitProcess(93);record(!moving||next==h);p=(BYTE*)api[family].lock(next);record(p!=NULL);record(api[family].size(next)>=40);
        for(i=0;i<13;i++)if(p[i]!=(BYTE)(i+9))copy=FALSE;
        for(i=13;i<40;i++)if(p[i]!=0)zero=FALSE;record(copy);record(zero);if(moving)api[family].unlock(next);
        SetLastError(0xcccc);h=api[family].resize(next,0,2);record(h!=NULL);record(GetLastError());
        if(h){record(api[family].flags(h));SetLastError(0xcccc);p=(BYTE*)api[family].lock(h);record(p!=NULL);record(p?0:GetLastError());record(api[family].free(h)==NULL);}
        else{record(0);record(0);record(0);record(1);}
    }
    for(family=0;family<2;family++){
        HANDLE h=api[family].alloc(0x40,32),next=api[family].resize(h,999,0x82);LPVOID p;
        record(next!=NULL);if(!next)ExitProcess(94);record(api[family].size(next)<999);p=api[family].lock(next);record(p!=next);record(api[family].flags(next));record(api[1-family].flags(next));api[family].unlock(next);record(api[family].free(next)==NULL);
    }
    file=CreateFileA("global-memory.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    if(file==INVALID_HANDLE_VALUE||!WriteFile(file,words,used*4,&wrote,NULL)||wrote!=used*4)ExitProcess(92);
    CloseHandle(file);ExitProcess(0);
}
