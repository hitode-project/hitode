/* Authored RIFF/MMIO ABI fixture. No game files, window or audio device. */
#include "win32_min.h"
typedef struct { DWORD flags,fcc,proc,error,task,size; char *buffer,*next,*endread,*endwrite;
    LONG bufoffset,diskoffset; DWORD info[3],reserved[2]; HANDLE handle; } MMIOINFO;
typedef struct { DWORD id,size,type,offset,flags; } CHUNK;
API HANDLE WINAPI mmioOpenA(char *,MMIOINFO *,DWORD);
API HANDLE WINAPI mmioOpenW(WORD *,MMIOINFO *,DWORD);
API UINT WINAPI mmioClose(HANDLE,UINT);
API LONG WINAPI mmioSeek(HANDLE,LONG,int);
API LONG WINAPI mmioRead(HANDLE,char *,LONG);
API LONG WINAPI mmioWrite(HANDLE,const char *,LONG);
API UINT WINAPI mmioDescend(HANDLE,CHUNK *,const CHUNK *,UINT);
API UINT WINAPI mmioAscend(HANDLE,CHUNK *,UINT);
API UINT WINAPI mmioGetInfo(HANDLE,MMIOINFO *,UINT);
API UINT WINAPI mmioSetInfo(HANDLE,const MMIOINFO *,UINT);
API DWORD WINAPI mmioStringToFOURCCA(const char *,UINT);
API DWORD WINAPI mmioStringToFOURCCW(const WORD *,UINT);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static DWORD report[512],count;
static BYTE wav[256],out[256];
static BYTE large[65537],large_out[65537];
static WORD wide_name[]={ 'f','i','x','t','u','r','e','.','w','a','v',0 },wide_four[]={'a','b',0};
static void emit(DWORD value) { report[count++]=value; }
static void zero(void *p,DWORD size) { BYTE *v=p; while(size--) *v++=0; }
static void word(DWORD at,DWORD value) { *(DWORD *)(wav+at)=value; }
#define FCC(a,b,c,d) ((DWORD)(a)|((DWORD)(b)<<8)|((DWORD)(c)<<16)|((DWORD)(d)<<24))
static void chunk(CHUNK *c,DWORD id,DWORD type) { zero(c,sizeof(*c)); c->id=id; c->type=type; }
static void descend(HANDLE h,CHUNK *c,CHUNK *parent,UINT flags) {
    UINT result=mmioDescend(h,c,parent,flags); emit(result);
    /* Output fields and file position after a failed search are undefined. */
    if(result) { emit(0); emit(0); emit(0); emit(0); emit(0); emit(0); }
    else { emit(c->id); emit(c->size); emit(c->type); emit(c->offset); emit(c->flags); emit(mmioSeek(h,0,1)); }
}
static HANDLE memory(DWORD size,DWORD flags) {
    MMIOINFO info; HANDLE h; zero(&info,sizeof(info)); info.fcc=FCC('M','E','M',' '); info.buffer=(char *)wav; info.size=size;
    h=mmioOpenA(NULL,&info,flags); emit(h!=0); emit(info.error); return h;
}
static void metadata(HANDLE h) {
    MMIOINFO info; zero(&info,sizeof(info)); emit(mmioGetInfo(h,&info,0));
    emit(info.flags); emit(info.fcc); emit(info.error); emit(info.size);
    emit((DWORD)(info.next-info.buffer)); emit((DWORD)(info.endread-info.buffer)); emit((DWORD)(info.endwrite-info.buffer)); emit(info.bufoffset); emit(info.diskoffset);
}
void mainCRTStartup(void) {
    CHUNK root,fmt,list,item,data,missing; MMIOINFO info; HANDLE h,h2,file; DWORD written,i;
    /* WAVE + odd JUNK + fmt + nested LIST/INAM + odd data. */
    word(0,FCC('R','I','F','F')); word(4,76); word(8,FCC('W','A','V','E'));
    word(12,FCC('J','U','N','K')); word(16,3); wav[20]='a'; wav[21]='b'; wav[22]='c';
    word(24,FCC('f','m','t',' ')); word(28,16); wav[32]=1; wav[34]=1; word(36,22050); word(40,44100); wav[44]=2; wav[46]=16;
    word(48,FCC('L','I','S','T')); word(52,14); word(56,FCC('I','N','F','O')); word(60,FCC('I','N','A','M')); word(64,1); wav[68]='X';
    word(70,FCC('d','a','t','a')); word(74,5); for(i=0;i<5;++i) wav[78+i]=(BYTE)(10+i);
    /* Correct root includes the terminal padding byte. */
    word(4,76); h=memory(84,0); metadata(h);
    chunk(&root,0,FCC('W','A','V','E')); descend(h,&root,NULL,0x20);
    chunk(&fmt,FCC('f','m','t',' '),0); descend(h,&fmt,&root,0x10);
    emit(mmioRead(h,(char *)out,16)); for(i=0;i<16;++i) emit(out[i]); metadata(h); emit(mmioAscend(h,&fmt,0)); emit(mmioSeek(h,0,1));
    chunk(&list,0,FCC('I','N','F','O')); descend(h,&list,&root,0x40);
    chunk(&item,0,0); descend(h,&item,&list,0); emit(mmioRead(h,(char *)out,1)); emit(out[0]); emit(mmioAscend(h,&item,0)); emit(mmioSeek(h,0,1)); emit(mmioAscend(h,&list,0));
    chunk(&data,FCC('d','a','t','a'),0); descend(h,&data,&root,0x10);
    /* A memory file observes caller writes after opening. */
    wav[78]=93; emit(mmioRead(h,(char *)out,5)); for(i=0;i<5;++i) emit(out[i]);
    emit(mmioSeek(h,-2,1)); metadata(h); zero(&info,sizeof(info)); emit(mmioGetInfo(h,&info,0)); info.next=info.buffer+79; emit(mmioSetInfo(h,&info,0)); emit(mmioSeek(h,0,1));
    emit(mmioSeek(h,0,2)); emit(mmioRead(h,(char *)out,5)); emit(mmioSeek(h,20,2)); emit(mmioRead(h,(char *)out,5)); emit(mmioSeek(h,-1,0)); emit(mmioSeek(h,0,1));
    emit(mmioSeek(h,12,0)); chunk(&missing,FCC('n','o','p','e'),0); descend(h,&missing,&root,0x10);
    emit(mmioClose(h,0)); emit(mmioClose(h,0)); emit(mmioStringToFOURCCA("xy",0)); emit(mmioStringToFOURCCA("abcdextra",0x10));
    /* Independent handle, malformed container, truncated headers and parent boundary. */
    h=memory(7,0); chunk(&root,0,FCC('W','A','V','E')); descend(h,&root,NULL,0x20); emit(mmioClose(h,0));
    h=memory(84,0); chunk(&root,0,FCC('W','A','V','E')); descend(h,&root,NULL,0x20); root.size=16; chunk(&data,FCC('d','a','t','a'),0); descend(h,&data,&root,0x10); emit(mmioClose(h,0));
    h=memory(84,2); emit(mmioSeek(h,78,0)); emit(mmioWrite(h,"QRSTU",5)); emit(mmioSeek(h,78,0)); emit(mmioRead(h,(char *)out,5)); for(i=0;i<5;++i) emit(out[i]); emit(mmioClose(h,0));
    file=CreateFileA("fixture.wav",0x40000000,0,NULL,2,0,0); if(file==0xffffffffu) ExitProcess(1); if(!WriteFile(file,wav,84,&written,NULL)||written!=84) ExitProcess(2); CloseHandle(file);
    h=mmioOpenA("fixture.wav",NULL,0); emit(h!=0); chunk(&root,0,FCC('W','A','V','E')); descend(h,&root,NULL,0x20); chunk(&fmt,FCC('f','m','t',' '),0); descend(h,&fmt,&root,0x10); emit(mmioRead(h,(char *)out,16)); for(i=0;i<16;++i) emit(out[i]); emit(mmioClose(h,0));
    h=memory(84,0); emit(mmioSeek(h,100,0)); emit(mmioSeek(h,0,1)); emit(mmioRead(h,(char *)out,1)); emit(mmioRead(h,(char *)out,0)); emit(mmioSeek(h,90,1)); emit(mmioSeek(h,0,1)); emit(mmioClose(h,0));
    zero(&info,sizeof(info)); info.fcc=FCC('M','E','M',' '); info.size=84; h=mmioOpenA(NULL,&info,0x1002); emit(h!=0); emit(info.error); metadata(h); emit(mmioSeek(h,0,2)); emit(mmioWrite(h,"OK",2)); metadata(h); emit(mmioSeek(h,0,0)); emit(mmioRead(h,(char *)out,2)); emit(out[0]); emit(out[1]); emit(mmioClose(h,0));
    /* Failed RIFF probing of an unrelated format must not claim WAV success. */
    word(0,FCC('O','g','g','S')); h=memory(84,0); chunk(&root,0,FCC('W','A','V','E')); descend(h,&root,NULL,0x20); emit(mmioClose(h,0));
    /* Large requested count stops at EOF; no request-sized host allocation. */
    for(i=0;i<65537;++i) large[i]=(BYTE)(i*17+13); zero(&info,sizeof(info)); info.fcc=FCC('M','E','M',' '); info.size=65537; info.buffer=(char *)large;
    h=mmioOpenA(NULL,&info,0); emit(h!=0); emit(mmioRead(h,(char *)large_out,0x7fffffff)); written=2166136261u; for(i=0;i<65537;++i) written=(written^large_out[i])*16777619u; emit(written); emit(mmioSeek(h,0,1)); emit(mmioClose(h,0));
    h=memory(84,0); h2=memory(84,0); emit(mmioSeek(h,40,0)); emit(mmioSeek(h2,0,1)); emit(mmioRead(h2,(char *)out,1)); emit(out[0]); emit(mmioSeek(h,0,1)); emit(mmioClose(h2,0)); emit(mmioClose(h,0));
    h=mmioOpenW(wide_name,NULL,0); emit(h!=0); chunk(&root,0,FCC('W','A','V','E')); descend(h,&root,NULL,0x20); emit(mmioClose(h,0)); emit(mmioStringToFOURCCW(wide_four,0x10));
    file=CreateFileA("mmio-report.bin",0x40000000,0,NULL,2,0,0); if(file==0xffffffffu) ExitProcess(3); if(!WriteFile(file,report,count*4,&written,NULL)||written!=count*4) ExitProcess(4); CloseHandle(file); ExitProcess(0);
}
