/* Authored AVI lifetime/ABI fixture. No window, device, or game files. */
#include "win32_min.h"
API void WINAPI AVIFileInit(void);
API void WINAPI AVIFileExit(void);
API LONG WINAPI AVIFileOpenA(void **, const char *, UINT, const void *);
API DWORD WINAPI AVIFileRelease(void *);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static DWORD report[32],count;
static void emit(DWORD value) { report[count++]=value; }
static void write(const char *name,const void *data,DWORD size) {
    DWORD written; HANDLE f=CreateFileA(name,0x40000000,0,NULL,2,0,0);
    if(f==0xffffffffu || !WriteFile(f,data,size,&written,NULL) || written!=size) ExitProcess(2);
    CloseHandle(f);
}
#define CALL_VOID(function) { volatile DWORD before,after; __asm { mov before,esp } function(); __asm { mov after,esp } emit(after-before); }
void mainCRTStartup(void) {
    CALL_VOID(AVIFileInit); CALL_VOID(AVIFileInit); CALL_VOID(AVIFileExit);
#ifdef AVI_OPEN_ERRORS
    const char *names[]={"missing.avi","missing.wav","corrupt.avi","empty.avi","truncated.avi","unknown.bin"};
    const BYTE truncated[]={ 'R','I','F','F',64,0,0,0,'A','V','I',' ' };
    const BYTE handler[16]={255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255};
    DWORD i; void *file; LONG result;
    write("corrupt.avi","not an AVI",10); write("empty.avi","",0);
    write("truncated.avi",truncated,sizeof(truncated)); write("unknown.bin","not an AVI",10);
    for(i=0;i<6;++i) {
        file=(void *)0x12345678; result=AVIFileOpenA(&file,names[i],0,NULL);
        emit((DWORD)result); emit(file==NULL); if(!result) AVIFileRelease(file);
    }
    file=(void *)0x12345678; result=AVIFileOpenA(&file,"missing.avi",0,handler);
    emit((DWORD)result); emit(file==NULL); if(!result) AVIFileRelease(file);
#endif
    CALL_VOID(AVIFileExit); CALL_VOID(AVIFileInit); CALL_VOID(AVIFileExit);
    write("avi-report.bin",report,count*4); ExitProcess(0);
}
