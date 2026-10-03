/* Native x86 CRT constructor walker oracle. No CRT startup or user DLLs. */
#include "win32_min.h"
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
API void *WINAPI EncodePointer(void *);
API void *WINAPI DecodePointer(void *);
typedef int (__cdecl *INIT)(void);
typedef int (__cdecl *INIT_E)(INIT *,INIT *);
typedef void (__cdecl *INIT_VOID)(INIT *,INIT *);
typedef void (__cdecl *LOCK)(int);
static LOCK lock_fn,unlock_fn;static HANDLE lock_started,lock_done;static volatile DWORD lock_visits;
static DWORD WINAPI lock_worker(void *unused){
    (void)unused;SetEvent(lock_started);lock_fn(8);++lock_visits;unlock_fn(8);SetEvent(lock_done);return 0;
}
static DWORD checks,order;static int failure;static BOOL mutate;
static INIT table[6];static DWORD report[128],count;
static void check(BOOL value){++checks;if(!value)ExitProcess(checks);}
static void record(DWORD value){report[count++]=value;}
static int __cdecl first(void){order=order*10+1;Sleep(1);if(mutate)table[3]=NULL;return 0;}
static int __cdecl second(void){order=order*10+2;return failure;}
static int __cdecl third(void){order=order*10+3;return 0;}
static void run(const char *module){
    HMODULE dll=LoadLibraryA(module);INIT_E init=(INIT_E)GetProcAddress(dll,"_initterm_e");
    INIT_VOID ordinary=(INIT_VOID)GetProcAddress(dll,"_initterm");int result;
    check(dll && init && ordinary);
    table[0]=NULL;table[1]=first;table[2]=NULL;table[3]=second;table[4]=third;table[5]=NULL;
    failure=0;order=0;result=init(table,table+6);check(result==0 && order==123);record((DWORD)result);record(order);
    failure=37;order=0;result=init(table,table+6);check(result==37 && order==12);record((DWORD)result);record(order);
    failure=-7;order=0;result=init(table,table+6);check(result==-7 && order==12);record((DWORD)result);record(order);
    order=0;ordinary(table,table+6);check(order==123);record(order);
    order=0;result=init(table,table);check(result==0 && order==0);record((DWORD)result);
    result=init(NULL,NULL);check(result==0);record((DWORD)result);
    mutate=1;order=0;result=init(table,table+6);check(result==0 && order==13);record((DWORD)result);record(order);mutate=0;
    {
        typedef void *(__cdecl *MALLOC)(DWORD);typedef void *(__cdecl *CALLOC)(DWORD,DWORD);
        typedef void *(__cdecl *REALLOC)(void *,DWORD);typedef void (__cdecl *FREE)(void *);
        MALLOC alloc=(MALLOC)GetProcAddress(dll,"malloc");CALLOC calloc=(CALLOC)GetProcAddress(dll,"calloc");
        REALLOC realloc=(REALLOC)GetProcAddress(dll,"realloc");FREE free=(FREE)GetProcAddress(dll,"free");BYTE *p;DWORD i;
#ifdef HOST_CRT_ALIASES
        if(module[5]!='t')alloc=(MALLOC)GetProcAddress(dll,"_malloc_crt");
#endif
        check(alloc && calloc && realloc && free);p=(BYTE *)calloc(16,4);check(p!=NULL);
        for(i=0;i<64;++i)check(p[i]==0);record(1);
        for(i=0;i<64;++i)p[i]=(BYTE)(i+17);p=(BYTE *)realloc(p,128);check(p!=NULL);
        for(i=0;i<64;++i)check(p[i]==(BYTE)(i+17));record(1);free(p);
        p=(BYTE *)alloc(128);check(p!=NULL);for(i=0;i<128;++i)p[i]=(BYTE)i;
        for(i=0;i<128;++i)check(p[i]==(BYTE)i);record(1);free(p);free(NULL);
        {
            typedef INIT (__cdecl *DLLONEXIT)(INIT,INIT **,INIT **);
            DLLONEXIT onexit=(DLLONEXIT)GetProcAddress(dll,"__dllonexit");INIT *begin=(INIT *)calloc(8,4),*end=begin;
            check(onexit && begin);failure=0;check(onexit(first,&begin,&end)==first);
            check(onexit(second,&begin,&end)==second);check(onexit(third,&begin,&end)==third);
            check(end-begin==3 && begin[0]==first && begin[1]==second && begin[2]==third);record(1);
            order=0;while(end!=begin){--end;(*end)();}check(order==321);record(order);free(begin);
        }
    }
    {
        typedef void *(__cdecl *POINTER)(void *);typedef void *(__cdecl *ENCODED_NULL)(void);
        void *raw=(void *)0x12345678,*encoded,*null_value;
#ifdef HOST_CRT_ALIASES
        if(module[5]!='t'){
            POINTER encode=(POINTER)GetProcAddress(dll,"_encode_pointer"),decode=(POINTER)GetProcAddress(dll,"_decode_pointer");
            ENCODED_NULL encoded_null=(ENCODED_NULL)GetProcAddress(dll,"_encoded_null");check(encode && decode && encoded_null);
            encoded=encode(raw);null_value=encoded_null();check(decode(encoded)==raw);record(1);
            check(decode(null_value)==NULL);record(1);check(DecodePointer(encoded)==raw);record(1);
        }else
#endif
        {
            encoded=EncodePointer(raw);null_value=EncodePointer(NULL);check(DecodePointer(encoded)==raw);record(1);
            check(DecodePointer(null_value)==NULL);record(1);check(DecodePointer(encoded)==raw);record(1);
        }
    }
    lock_fn=(LOCK)GetProcAddress(dll,"_lock");unlock_fn=(LOCK)GetProcAddress(dll,"_unlock");check(lock_fn && unlock_fn);
    {
        HANDLE thread;lock_visits=0;lock_started=CreateEventA(NULL,0,0,NULL);lock_done=CreateEventA(NULL,0,0,NULL);check(lock_started && lock_done);
        lock_fn(8);lock_fn(8);thread=CreateThread(NULL,0,lock_worker,NULL,0,NULL);check(thread!=0);
        check(WaitForSingleObject(lock_started,1000)==0);unlock_fn(8);
        check(WaitForSingleObject(lock_done,0)==258 && lock_visits==0);unlock_fn(8);
        check(WaitForSingleObject(lock_done,1000)==0 && lock_visits==1);record(lock_visits);
        check(WaitForSingleObject(thread,1000)==0);CloseHandle(thread);CloseHandle(lock_started);CloseHandle(lock_done);
    }
    {
        typedef int *(__cdecl *GLOBAL)(void);GLOBAL fmode=(GLOBAL)GetProcAddress(dll,"__p__fmode"),commode=(GLOBAL)GetProcAddress(dll,"__p__commode");
        int *p,*q,old;check(fmode && commode);p=fmode();q=commode();check(p && q && p!=q);
        old=*p;*p=0x8000;check(fmode()==p && *fmode()==0x8000);*p=old;record(1);
        old=*q;*q=1;check(commode()==q && *commode()==1);*q=old;record(1);
    }
    FreeLibrary(dll);
}
void mainCRTStartup(void){
    DWORD written;HANDLE file;
    run("msvcrt.dll");
#ifdef HOST_CRT_ALIASES
    run("msvcr80.dll");run("msvcr90.dll");
#endif
    file=CreateFileA("crt-init.bin",0x40000000u,0,NULL,2,0,0);check(file!=0xffffffffu);
    check(WriteFile(file,report,count*4,&written,NULL) && written==count*4);CloseHandle(file);ExitProcess(0);
}
