/* Authored DOS/FILETIME conversion oracle; no game or device execution. */
#include "win32_min.h"
typedef struct {DWORD low,high;} FT;
API BOOL WINAPI FileTimeToDosDateTime(const FT*,WORD*,WORD*);
API BOOL WINAPI DosDateTimeToFileTime(WORD,WORD,FT*);
API void WINAPI SetLastError(DWORD);
API DWORD WINAPI GetLastError(void);
API HANDLE WINAPI CreateFileA(const char*,DWORD,DWORD,void*,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void*,DWORD,DWORD*,void*);
static const FT filetimes[]={
{0x00000000u,0x00000000u},
{0xe0a45300u,0x01a8e79fu},
{0xe13ce980u,0x01a8e79fu},
{0xe1d57fffu,0x01a8e79fu},
{0xe1d58000u,0x01a8e79fu},
{0xe1d58001u,0x01a8e79fu},
{0xe26e167fu,0x01a8e79fu},
{0xe26e1680u,0x01a8e79fu},
{0xe306acffu,0x01a8e79fu},
{0xe306ad00u,0x01a8e79fu},
{0xe306ad01u,0x01a8e79fu},
{0xe39f4380u,0x01a8e79fu},
{0xe437d9ffu,0x01a8e79fu},
{0xa834c000u,0x01a915fcu},
{0xebcc8000u,0x01bf8247u},
{0x8e646980u,0x01c0a1e2u},
{0x24d4a980u,0x01bf53ebu},
{0x3d2aa980u,0x022f9fc0u},
{0x66d29300u,0x023868b8u},
{0x676b2980u,0x023868b8u},
{0x6803c000u,0x023868b8u},
{0x6803bfffu,0x023868b8u},
{0xffffffffu,0x7fffffffu},
{0x00000000u,0x80000000u},
{0xffffffffu,0xffffffffu},
};
static const WORD packed[][2]={
{0x0021,0x0000},
{0x005d,0x0000},
{0x285d,0x645c},
{0xff9f,0xbf7d},
{0x005e,0x0000},
{0x2a5d,0x0000},
{0xf05d,0x0000},
{0xf09f,0x0000},
{0x0001,0x0000},
{0x01a1,0x0000},
{0x0020,0x0000},
{0x0021,0xc000},
{0x0021,0xf800},
{0x0021,0x0780},
{0x0021,0x07e0},
{0x0021,0x001e},
{0x0021,0x001f},
{0x0021,0xffff},
{0x0000,0x0000},
{0xffff,0x0000},
{0x003f,0x001d},
};
static DWORD words[1024],n;
#define REC(v) words[n++]=(DWORD)(v)
void mainCRTStartup(void){
 unsigned i;DWORD wrote;HANDLE file;
 for(i=0;i<sizeof(filetimes)/sizeof(filetimes[0]);i++){
  WORD date=0xcafe,time=0xbeef;BOOL result;SetLastError(0x13572468);result=FileTimeToDosDateTime(&filetimes[i],&date,&time);
  REC(filetimes[i].low);REC(filetimes[i].high);REC(result);REC(GetLastError());REC(date);REC(time);
 }
 for(i=0;i<sizeof(packed)/sizeof(packed[0]);i++){
  FT out={0xdeadbeef,0xcafebabe};BOOL result;SetLastError(0x13572468);result=DosDateTimeToFileTime(packed[i][0],packed[i][1],&out);
  REC(packed[i][0]);REC(packed[i][1]);REC(result);REC(GetLastError());REC(out.low);REC(out.high);
 }
 REC(0xd05f17e0);file=CreateFileA("dos-filetime.bin",0x40000000,0,0,2,0,0);
 if(file==0xffffffffu||!WriteFile(file,words,n*4,&wrote,0)||wrote!=n*4)ExitProcess(90);CloseHandle(file);ExitProcess(0);
}
