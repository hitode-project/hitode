/* Authored local regular-file coherence. No game/device/window. */
#include "win32_min.h"
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI ReadFile(HANDLE,void *,DWORD,DWORD *,void *);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
API DWORD WINAPI SetFilePointer(HANDLE,LONG,LONG *,DWORD);
API HANDLE WINAPI CreateFileMappingA(HANDLE,void *,DWORD,DWORD,DWORD,const char *);
API void *WINAPI MapViewOfFile(HANDLE,DWORD,DWORD,DWORD,DWORD);
API BOOL WINAPI FlushViewOfFile(const void *,DWORD);
API BOOL WINAPI UnmapViewOfFile(const void *);
static DWORD initial[3072],words[128],used;
static void emit(DWORD value) {words[used++]=value;}
static void existing_file(void) {
    HANDLE ro_file,writer,section;volatile DWORD *view,*copy;DWORD count,value;
    ro_file=CreateFileA("coherence-base.bin",0x80000000,3,NULL,3,0,0);
    section=CreateFileMappingA(ro_file,NULL,2,0,0,NULL);view=MapViewOfFile(section,4,0,0,8192);copy=MapViewOfFile(section,1,0,0,8192);
    if(!view||!copy)ExitProcess(14);emit(view[0]);emit(copy[0]);copy[0]=0xcafe0001;
    SetFilePointer(ro_file,17,NULL,0);
    writer=CreateFileA("coherence-base.bin",0x40000000,3,NULL,3,0,0);if(writer==0xffffffffu)ExitProcess(15);
    value=0xfacecafe;emit(WriteFile(writer,&value,4,&count,NULL));emit(count);emit(view[0]);emit(copy[0]);emit(SetFilePointer(ro_file,0,NULL,1));
    emit(CloseHandle(writer));emit(CloseHandle(ro_file));emit(CloseHandle(section));emit(view[0]);emit(UnmapViewOfFile((const void *)view));emit(UnmapViewOfFile((const void *)copy));
}
void mainCRTStartup(void) {
    HANDLE file,section,other,report;DWORD count,value,i;BYTE bytes[8];
    volatile DWORD *rw,*ro,*copy,*separate;volatile BYTE *shared_bytes,*copy_bytes;
    file=CreateFileA("coherent.bin",0xc0000000,3,NULL,2,0,0);if(file==0xffffffffu)ExitProcess(10);
    for(i=0;i<3072;i++)initial[i]=0x10000000+i;
    if(!WriteFile(file,initial,sizeof(initial),&count,NULL))ExitProcess(11);
    section=CreateFileMappingA(file,NULL,4,0,0,NULL);
    other=CreateFileMappingA(file,NULL,2,0,0,NULL);
    rw=MapViewOfFile(section,2,0,0,12288);ro=MapViewOfFile(section,4,0,0,12288);
    copy=MapViewOfFile(section,1,0,0,12288);separate=MapViewOfFile(other,4,0,0,12288);
    if(!rw||!ro||!copy||!separate)ExitProcess(12);
    emit(ro[0]);emit(copy[0]);emit(ro[1024]);emit(copy[1024]);emit(separate[0]);
    copy[0]=0xc0c0c0c0;rw[1]=0xaaaaaaaa;rw[1027]=0xbbbbbbbb;
    SetFilePointer(file,4,NULL,0);emit(ReadFile(file,&value,4,&count,NULL));emit(value);emit(count);emit(SetFilePointer(file,0,NULL,1));
    value=0x12345678;SetFilePointer(file,0,NULL,0);emit(WriteFile(file,&value,4,&count,NULL));emit(count);emit(SetFilePointer(file,0,NULL,1));
    emit(ro[0]);emit(separate[0]);emit(copy[0]);emit(ro[1]);emit(copy[1]);
    for(i=0;i<8;i++)bytes[i]=(BYTE)(0xd0+i);
    SetFilePointer(file,4094,NULL,0);emit(WriteFile(file,bytes,8,&count,NULL));emit(count);emit(SetFilePointer(file,0,NULL,1));
    shared_bytes=(volatile BYTE *)ro;copy_bytes=(volatile BYTE *)copy;
    for(i=4094;i<4102;i++)emit(shared_bytes[i]);
    for(i=4094;i<4102;i++)emit(copy_bytes[i]);
    emit(ro[1027]);emit(copy[1027]);
    emit(FlushViewOfFile((const void *)rw,0));emit(SetFilePointer(file,0,NULL,1));
    SetFilePointer(file,4108,NULL,0);emit(ReadFile(file,&value,4,&count,NULL));emit(value);emit(count);
    emit(CloseHandle(section));emit(CloseHandle(other));emit(CloseHandle(file));
    emit(ro[0]);emit(copy[0]);
    emit(UnmapViewOfFile((const void *)rw));emit(UnmapViewOfFile((const void *)ro));emit(UnmapViewOfFile((const void *)copy));emit(UnmapViewOfFile((const void *)separate));
    existing_file();report=CreateFileA("file-coherence.bin",0x40000000,0,NULL,2,0,0);
    if(report==0xffffffffu||!WriteFile(report,words,used*4,&count,NULL))ExitProcess(13);
    CloseHandle(report);ExitProcess(0);
}
