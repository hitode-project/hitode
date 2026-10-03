/* Native data-section oracle. All disk files are authored by this fixture. */
#include "win32_min.h"
API HANDLE WINAPI CreateFileMappingA(HANDLE,void*,DWORD,DWORD,DWORD,const char*);
API HANDLE WINAPI CreateFileMappingW(HANDLE,void*,DWORD,DWORD,DWORD,const WORD*);
API HANDLE WINAPI OpenFileMappingA(DWORD,BOOL,const char*);
API HANDLE WINAPI OpenFileMappingW(DWORD,BOOL,const WORD*);
API void * WINAPI MapViewOfFile(HANDLE,DWORD,DWORD,DWORD,DWORD);
API void * WINAPI MapViewOfFileEx(HANDLE,DWORD,DWORD,DWORD,DWORD,void*);
API BOOL WINAPI FlushViewOfFile(const void*,DWORD);
API BOOL WINAPI UnmapViewOfFile(const void*);
API void * WINAPI VirtualAlloc(void*,DWORD,DWORD,DWORD);
API BOOL WINAPI VirtualFree(void*,DWORD,DWORD);
API BOOL WINAPI VirtualProtect(void*,DWORD,DWORD,DWORD*);
API DWORD WINAPI VirtualQuery(const void*,void*,DWORD);
API HANDLE WINAPI CreateFileA(const char*,DWORD,DWORD,void*,DWORD,DWORD,HANDLE);
API BOOL WINAPI ReadFile(HANDLE,void*,DWORD,DWORD*,void*);
API BOOL WINAPI WriteFile(HANDLE,const void*,DWORD,DWORD*,void*);
API DWORD WINAPI SetFilePointer(HANDLE,LONG,LONG*,DWORD);
API DWORD WINAPI GetFileSize(HANDLE,DWORD*);
API void WINAPI SetLastError(DWORD);
API DWORD WINAPI GetLastError(void);
API HANDLE WINAPI CreateEventW(void*,BOOL,BOOL,const WORD*);
API HANDLE WINAPI CreateMutexA(void*,BOOL,const char*);
API HANDLE WINAPI CreateSemaphoreA(void*,LONG,LONG,const char*);
API BOOL WINAPI ReleaseSemaphore(HANDLE,LONG,LONG*);
API BOOL WINAPI ReleaseMutex(HANDLE);
API BOOL WINAPI ResetEvent(HANDLE);
API BOOL WINAPI SetEvent(HANDLE);
API DWORD WINAPI WaitForMultipleObjects(DWORD,const HANDLE*,BOOL,DWORD);
static const char name[]="Local\\AdvFileMappingFixture";
static const WORD namew[]={'L','o','c','a','l','\\','A','d','v','F','i','l','e','M','a','p','p','i','n','g','F','i','x','t','u','r','e',0};
static DWORD report[512],used,block[1024];
static void emit(DWORD value){report[used++]=value;}
static void query(const void *view){DWORD info[7];emit(VirtualQuery(view,info,28));emit(info[2]);emit(info[3]);emit(info[5]);emit(info[6]);}
static void errors(void){
    HANDLE h;void *p,*fixed;DWORD old;
    SetLastError(0);emit(OpenFileMappingA(4,0,"AdvMappingMissing")==0);emit(GetLastError());
    SetLastError(0);emit(CreateFileMappingA((HANDLE)-1,0,4,0,0,0)==0);emit(GetLastError());
    h=CreateFileMappingA((HANDLE)-1,0,2,0,65536,0);if(!h)ExitProcess(10);
    SetLastError(0);emit(MapViewOfFile(h,2,0,0,4096)==0);emit(GetLastError());
    SetLastError(0);emit(MapViewOfFile(h,4,0,4096,4096)==0);emit(GetLastError());
    p=MapViewOfFile(h,4,0,0,4096);if(!p)ExitProcess(11);
    SetLastError(0);emit(VirtualProtect(p,4096,4,&old));emit(GetLastError());
    emit(UnmapViewOfFile(p));
    fixed=VirtualAlloc(0,65536,0x3000,4);if(!fixed)ExitProcess(12);emit(VirtualFree(fixed,0,0x8000));
    p=MapViewOfFileEx(h,4,0,0,4096,fixed);emit(p==fixed);if(!p)ExitProcess(13);
    SetLastError(0);emit(MapViewOfFileEx(h,4,0,0,4096,fixed)==0);emit(GetLastError());
    SetLastError(0);emit(MapViewOfFileEx(h,4,0,0,4096,(char*)fixed+1)==0);emit(GetLastError());
    emit(UnmapViewOfFile(p));emit(CloseHandle(h));
}
static void anonymous(void){
    HANDLE h,a,b,last;volatile DWORD *rw,*ro,*copy;DWORD old;
    h=CreateFileMappingA((HANDLE)-1,0,4,0,131072,name);emit(h!=0);emit(GetLastError());if(!h)ExitProcess(20);
    a=CreateFileMappingW((HANDLE)-1,0,4,0,65536,namew);emit(a!=0);emit(GetLastError());
    b=OpenFileMappingW(4,0,namew);emit(b!=0);if(!a||!b)ExitProcess(21);
    rw=MapViewOfFile(h,6,0,0,8192);ro=MapViewOfFile(b,4,0,0,8192);copy=MapViewOfFile(a,1,0,0,8192);
    if(!rw||!ro||!copy)ExitProcess(22);emit(ro[0]);emit(copy[0]);query((const void*)copy);
    rw[0]=0x12345678;emit(ro[0]);emit(copy[0]);copy[0]=0x10203040;rw[0]=0x87654321;
    emit(ro[0]);emit(copy[0]);query((const void*)copy);
    rw[1024]=0x11223344;emit(copy[1024]);copy[1024]=0xaabbccdd;rw[1024]=0x55667788;emit(copy[1024]);emit(ro[1024]);
    emit(VirtualProtect((void*)rw,4096,2,&old));emit(old);emit(VirtualProtect((void*)rw,4096,4,&old));emit(old);
    emit(CloseHandle(h));emit(CloseHandle(a));emit(CloseHandle(b));
    SetLastError(0);last=OpenFileMappingA(4,0,name);emit(last==0);emit(GetLastError());if(last)ExitProcess(23);
    last=CreateFileMappingA((HANDLE)-1,0,4,0,4096,name);emit(last!=0);emit(GetLastError());if(!last)ExitProcess(24);
    {HANDLE c=OpenFileMappingA(1,0,name);void *v;emit(c!=0);if(!c)ExitProcess(25);
        v=MapViewOfFile(c,1,0,0,4096);emit(v!=0);if(v){emit(*(DWORD*)v);emit(UnmapViewOfFile(v));}emit(CloseHandle(c));}
    emit(CloseHandle(last));emit(ro[0]);emit(FlushViewOfFile((const void*)copy,0));
    emit(UnmapViewOfFile((const void*)ro));emit(UnmapViewOfFile((const void*)copy));emit(UnmapViewOfFile((const void*)rw));
    SetLastError(0);emit(OpenFileMappingA(4,0,name)==0);emit(GetLastError());
}
static void rep_aliases(BOOL serial){
    HANDLE h=CreateFileMappingA((HANDLE)-1,0,4,0,8192,0);
    volatile BYTE *a=MapViewOfFile(h,2,0,0,8192),*b=MapViewOfFile(h,2,0,0,8192),*copy;
    DWORD i;BYTE *source,*destination;
    if(!a||!b)ExitProcess(26);
    for(i=0;i<64;++i)a[i]=(BYTE)i;
    source=(BYTE*)a;destination=(BYTE*)b+1;
    if(serial){
        __asm {
            cld
            mov esi,source
            mov edi,destination
            mov ecx,32
serial_byte_forward:
            movsb
            loop serial_byte_forward
        }
    }else{ __asm {
        cld
        mov esi,source
        mov edi,destination
        mov ecx,32
        rep movsb
    }}
    emit(b[1]);emit(b[16]);emit(b[32]);emit(b[33]);
    for(i=0;i<64;++i)a[i]=(BYTE)i;
    source=(BYTE*)a+32;destination=(BYTE*)b+31;
    if(serial){
        __asm {
            std
            mov esi,source
            mov edi,destination
            mov ecx,32
serial_byte_backward:
            movsb
            loop serial_byte_backward
            cld
        }
    }else{ __asm {
        std
        mov esi,source
        mov edi,destination
        mov ecx,32
        rep movsb
        cld
    }}
    emit(b[0]);emit(b[15]);emit(b[31]);emit(b[33]);
    for(i=0;i<64;++i)((volatile DWORD*)a)[i]=0x11220000+i;
    source=(BYTE*)a;destination=(BYTE*)b+4;
    if(serial){
        __asm {
            cld
            mov esi,source
            mov edi,destination
            mov ecx,32
serial_dword_forward:
            movsd
            loop serial_dword_forward
        }
    }else{ __asm {
        cld
        mov esi,source
        mov edi,destination
        mov ecx,32
        rep movsd
    }}
    emit(((volatile DWORD*)b)[1]);emit(((volatile DWORD*)b)[16]);emit(((volatile DWORD*)b)[32]);
    for(i=0;i<8192;++i)a[i]=(BYTE)i;
    copy=MapViewOfFile(h,1,0,0,8192);if(!copy)ExitProcess(27);
    source=(BYTE*)a+4090;destination=(BYTE*)copy+4091;
    if(serial){
        __asm {
            cld
            mov esi,source
            mov edi,destination
            mov ecx,16
serial_copy_forward:
            movsb
            loop serial_copy_forward
        }
    }else{ __asm {
        cld
        mov esi,source
        mov edi,destination
        mov ecx,16
        rep movsb
    }}
    emit(copy[4091]);emit(copy[4096]);emit(copy[4106]);emit(a[4091]);emit(a[4096]);
    emit(UnmapViewOfFile((const void*)a));emit(UnmapViewOfFile((const void*)b));emit(UnmapViewOfFile((const void*)copy));emit(CloseHandle(h));
}
static void files(void){
    HANDLE f,m,second,read,empty;volatile DWORD *a,*b,*copy;DWORD n,i,high=0,value;
    f=CreateFileA("mapping-source.bin",0xc0000000,3,0,2,0,0);if(f==(HANDLE)-1)ExitProcess(30);
    for(i=0;i<1024;++i)block[i]=i^0x5a5a5a5a;
    for(i=0;i<17;++i)if(!WriteFile(f,block,sizeof(block),&n,0)||n!=sizeof(block))ExitProcess(31);
    emit(SetFilePointer(f,7,0,0));m=CreateFileMappingA(f,0,4,0,81920,0);emit(m!=0);if(!m)ExitProcess(32);
    emit(GetFileSize(f,&high));emit(high);emit(SetFilePointer(f,0,0,1));
    a=MapViewOfFile(m,2,0,65536,8192);if(!a)ExitProcess(33);emit(SetFilePointer(f,0,0,1));emit(a[0]);emit(SetFilePointer(f,0,0,1));
    second=CreateFileMappingW(f,0,4,0,0,0);b=MapViewOfFile(second,4,0,65536,8192);copy=MapViewOfFile(m,1,0,65536,8192);
    if(!b||!copy)ExitProcess(34);a[0]=0x1234abcd;emit(b[0]);emit(copy[0]);copy[0]=0xffffffff;
    emit(FlushViewOfFile((const void*)copy,0));emit(FlushViewOfFile((const void*)a+0,4));emit(SetFilePointer(f,0,0,1));
    emit(SetFilePointer(f,65536,0,0));emit(ReadFile(f,&value,4,&n,0));emit(value);emit(n);emit(SetFilePointer(f,0,0,1));
    emit(CloseHandle(f));emit(CloseHandle(m));emit(CloseHandle(second));emit(b[0]);
    emit(UnmapViewOfFile((const void*)a));emit(UnmapViewOfFile((const void*)b));emit(UnmapViewOfFile((const void*)copy));
    read=CreateFileA("mapping-source.bin",0x80000000,3,0,3,0,0);m=CreateFileMappingA(read,0,2,0,0,0);
    a=MapViewOfFile(m,4,0,65536,0);if(!a)ExitProcess(35);emit(a[0]);emit(CloseHandle(read));emit(CloseHandle(m));emit(FlushViewOfFile((const void*)a,0));emit(UnmapViewOfFile((const void*)a));
    empty=CreateFileA("mapping-empty.bin",0xc0000000,3,0,2,0,0);SetLastError(0);emit(CreateFileMappingA(empty,0,2,0,0,0)==0);emit(GetLastError());emit(CloseHandle(empty));
}
static void namespace_cases(void){
    static const WORD ew[]={'A','d','v','M','a','p','E','v','e','n','t',0};HANDLE e,e2,m,s,mutex,hs[2];
    e=CreateEventA(0,1,0,"AdvMapEvent");emit(e!=0);emit(GetLastError());e2=CreateEventW(0,0,1,ew);emit(e2!=0);emit(GetLastError());
    emit(WaitForSingleObject(e2,0));emit(SetEvent(e2));emit(WaitForSingleObject(e,0));emit(ResetEvent(e));emit(WaitForSingleObject(e2,0));
    hs[0]=e;hs[1]=e2;SetLastError(0);emit(WaitForMultipleObjects(2,hs,1,0));emit(GetLastError());
    SetLastError(0);emit(CreateFileMappingA((HANDLE)-1,0,4,0,4096,"AdvMapEvent")==0);emit(GetLastError());
    SetLastError(0);emit(OpenFileMappingA(4,0,"AdvMapEvent")==0);emit(GetLastError());
    emit(CloseHandle(e));emit(SetEvent(e2));emit(WaitForSingleObject(e2,0));emit(CloseHandle(e2));
    m=CreateFileMappingA((HANDLE)-1,0,4,0,4096,"AdvMapEvent");emit(m!=0);emit(GetLastError());
    SetLastError(0);emit(CreateEventA(0,1,0,"AdvMapEvent")==0);emit(GetLastError());
    SetLastError(0);emit(CreateMutexA(0,0,"AdvMapEvent")==0);emit(GetLastError());
    SetLastError(0);emit(CreateSemaphoreA(0,0,1,"AdvMapEvent")==0);emit(GetLastError());emit(CloseHandle(m));
    mutex=CreateMutexA(0,1,"AdvMapMutex");e=CreateMutexA(0,0,"AdvMapMutex");emit(e!=0);emit(GetLastError());emit(WaitForSingleObject(e,0));emit(ReleaseMutex(mutex));emit(CloseHandle(mutex));emit(ReleaseMutex(e));emit(CloseHandle(e));
    s=CreateSemaphoreA(0,0,2,"AdvMapSemaphore");e=CreateSemaphoreA(0,1,3,"AdvMapSemaphore");emit(e!=0);emit(GetLastError());emit(WaitForSingleObject(e,0));emit(ReleaseSemaphore(s,1,0));emit(WaitForSingleObject(e,0));emit(CloseHandle(s));emit(CloseHandle(e));
}
void mainCRTStartup(void){HANDLE out;DWORD n;errors();anonymous();rep_aliases(0);files();namespace_cases();
    out=CreateFileA("file-mapping.bin",0x40000000,0,0,2,0,0);if(out==(HANDLE)-1||!WriteFile(out,report,used*4,&n,0)||n!=used*4)ExitProcess(40);CloseHandle(out);
    /* Separate raw serial oracle; never rewrite the native bulk REP report. */
    used=0;rep_aliases(1);
    out=CreateFileA("file-mapping-serial.bin",0x40000000,0,0,2,0,0);if(out==(HANDLE)-1||!WriteFile(out,report,used*4,&n,0)||n!=used*4)ExitProcess(41);CloseHandle(out);ExitProcess(0);
}
