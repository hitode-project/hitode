/* Private allocations only: COM task memory and BSTR lifetime/ABI oracle. */
#include "win32_min.h"
API void *WINAPI CoTaskMemAlloc(DWORD);
API void *WINAPI CoTaskMemRealloc(void *,DWORD);
API void WINAPI CoTaskMemFree(void *);
API WORD *WINAPI SysAllocString(const WORD *);
API WORD *WINAPI SysAllocStringLen(const WORD *,UINT);
API WORD *WINAPI SysAllocStringByteLen(const char *,UINT);
API int WINAPI SysReAllocString(WORD **,const WORD *);
API int WINAPI SysReAllocStringLen(WORD **,const WORD *,UINT);
API void WINAPI SysFreeString(WORD *);
API UINT WINAPI SysStringLen(const WORD *);
API UINT WINAPI SysStringByteLen(const WORD *);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static DWORD report[256],count;
static void emit(DWORD value){report[count++]=value;}
static void string(WORD *p) {
    DWORD n=SysStringByteLen(p),i;
    emit(p!=NULL);emit(SysStringLen(p));emit(n);
    if(p){emit(((DWORD *)p)[-1]);for(i=0;i<n+2;++i)emit(((BYTE *)p)[i]);}
}
void mainCRTStartup(void) {
    BYTE *p,*q; WORD *s,*t; DWORD i,written; HANDLE file; HMODULE module;
    const WORD source[]={0x6f22,0,0x5b57,0};
    const char raw[]={1,2,0,3,4};
    const char *names[]={"SysAllocString","SysReAllocString","SysAllocStringLen","SysReAllocStringLen","SysFreeString","SysStringLen","SysAllocStringByteLen","SysStringByteLen"};
    const DWORD ordinals[]={2,3,4,5,6,7,150,149};
    module=LoadLibraryA("oleaut32.dll");
    for(i=0;i<8;++i) emit(GetProcAddress(module,names[i])==GetProcAddress(module,(const char *)ordinals[i]));
    p=CoTaskMemAlloc(0);emit(p!=NULL);CoTaskMemFree(p);CoTaskMemFree(NULL);
    p=CoTaskMemRealloc(NULL,16);emit(p!=NULL);
    for(i=0;i<16;++i)p[i]=(BYTE)(i*3);
    q=CoTaskMemRealloc(p,64);emit(q!=NULL);for(i=0;i<16;++i)emit(q[i]);
    q=CoTaskMemRealloc(q,8);emit(q!=NULL);for(i=0;i<8;++i)emit(q[i]);
    emit(CoTaskMemRealloc(q,0)==NULL);
    s=SysAllocString(NULL);string(s);SysFreeString(s);
    s=SysAllocString((const WORD *)L"");string(s);SysFreeString(s);
    s=SysAllocString(source);string(s);SysFreeString(s);
    s=SysAllocStringLen(source,3);string(s);
    emit(SysReAllocStringLen(&s,s+1,2));string(s);
    emit(SysReAllocString(&s,(const WORD *)L"tail"));string(s);
    emit(SysReAllocString(&s,NULL));string(s);
    emit(SysReAllocStringLen(&s,source,3));string(s);SysFreeString(s);
    s=SysAllocStringByteLen(raw,5);string(s);SysFreeString(s);
    s=SysAllocStringLen(NULL,0);string(s);SysFreeString(s);
    t=SysAllocString((const WORD *)L"keep");s=t;
    emit(SysReAllocStringLen(&s,source,0xffffffffu));emit(s==t);string(s);SysFreeString(s);
    file=CreateFileA("ole-memory.bin",0x40000000,0,NULL,2,0,0);
    if(file==0xffffffffu||!WriteFile(file,report,count*4,&written,NULL)||written!=count*4)ExitProcess(4);
    CloseHandle(file);ExitProcess(0);
}
