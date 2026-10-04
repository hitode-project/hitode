/* Authored private regular files only. No window, audio or system setting. */
#include "win32_min.h"
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI ReadFile(HANDLE,void *,DWORD,DWORD *,void *);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
API DWORD WINAPI SetFilePointer(HANDLE,LONG,LONG *,DWORD);
API DWORD WINAPI GetFileSize(HANDLE,DWORD *);
API DWORD WINAPI GetFullPathNameA(const char *,DWORD,char *,char **);
API HANDLE WINAPI CreateFileMappingA(HANDLE,void *,DWORD,DWORD,DWORD,const char *);
API void *WINAPI MapViewOfFile(HANDLE,DWORD,DWORD,DWORD,DWORD);
API BOOL WINAPI UnmapViewOfFile(const void *);
API void WINAPI SetLastError(DWORD);
API DWORD WINAPI GetLastError(void);
API BOOL WINAPI CopyFileA(const char *,const char *,BOOL);
API BOOL WINAPI WritePrivateProfileStringA(const char *,const char *,const char *,const char *);
static DWORD words[256],used;
static BYTE bytes[8192];
static char profile_path[]="mutation-0.ini";
static char profile_index='0';
static void emit(DWORD value){words[used++]=value;}
static void put(const char *path,const void *data,DWORD length){
    DWORD count;HANDLE file=CreateFileA(path,0x40000000,3,NULL,2,0,0);
    if(file==0xffffffffu||!WriteFile(file,data,length,&count,NULL)||count!=length)ExitProcess(10);
    CloseHandle(file);
}
static void probe(BOOL profile,DWORD length,DWORD protection,BOOL keep_view){
    HANDLE file,section;volatile BYTE *view;DWORD count,value;BOOL result;
    const char *target=profile?profile_path:"mutation-target.bin";
    char absolute_path[1024];
    DWORD i;
    for(i=0;i<8192;i++)bytes[i]=0x11;
    if(profile){profile_path[9]=profile_index++;put(target,"[s]\r\nk=old\r\n",12);}else{put(target,bytes,4096);}
    if(!GetFullPathNameA(target,sizeof(absolute_path),absolute_path,NULL))ExitProcess(13);
    target=absolute_path;
    file=CreateFileA(target,0xc0000000,3,NULL,3,0,0);
    section=CreateFileMappingA(file,NULL,protection,0,0,NULL);
    view=MapViewOfFile(section,protection==4?2:4,0,0,0);
    if(file==0xffffffffu||!section||!view)ExitProcess(11);
    emit(view[0]);
    if(!keep_view){UnmapViewOfFile((const void *)view);view=NULL;}
    for(i=0;i<8192;i++)bytes[i]=0x22;
    if(!profile)put("mutation-source.bin",bytes,length);
    SetLastError(0);
    result=profile?WritePrivateProfileStringA("s","k",length==0?"x":length==1?"new":"long-value",target):CopyFileA("mutation-source.bin",target,0);
    emit(result);emit(GetLastError());
    SetLastError(0);emit(profile?WritePrivateProfileStringA(NULL,NULL,NULL,target):0);emit(GetLastError());
    emit(GetFileSize(file,NULL));
    SetFilePointer(file,profile?7:0,NULL,0);value=0;ReadFile(file,&value,4,&count,NULL);emit(value);
    emit(view?(profile?*(volatile DWORD *)(view+7):view[0]):0);
    if(view)UnmapViewOfFile((const void *)view);
    CloseHandle(section);CloseHandle(file);
    SetLastError(0);emit(profile?WritePrivateProfileStringA(NULL,NULL,NULL,target):0);emit(GetLastError());
    file=CreateFileA(target,0x80000000,3,NULL,3,0,0);emit(GetFileSize(file,NULL));
    SetFilePointer(file,profile?7:0,NULL,0);value=0;ReadFile(file,&value,4,&count,NULL);emit(value);CloseHandle(file);
}
void mainCRTStartup(void){
    DWORD i,count;HANDLE report;
    for(i=0;i<3;i++)probe(0,i==0?4096:i==1?2048:8192,2,1);
    for(i=0;i<3;i++)probe(0,i==0?4096:i==1?2048:8192,4,1);
    probe(0,4096,2,0);
    for(i=0;i<3;i++)probe(1,i,2,1);
    for(i=0;i<3;i++)probe(1,i,4,1);
    probe(1,1,2,0);
    put("mutation-target.bin","same",4);SetLastError(0);
    emit(CopyFileA("mutation-target.bin","mutation-target.bin",0));emit(GetLastError());
    report=CreateFileA("file-mutation.bin",0x40000000,0,NULL,2,0,0);
    if(report==0xffffffffu||!WriteFile(report,words,used*4,&count,NULL))ExitProcess(12);
    CloseHandle(report);ExitProcess(0);
}
