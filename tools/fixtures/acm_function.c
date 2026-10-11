/* Authored x86 function-driver differential fixture.
 * Build with build-mmio.ps1; no inputs. Output acm-function.bin contains only
 * API results and boolean checks over synthetic formats, buffers and handles. */
#include "win32_min.h"
API DWORD WINAPI acmDriverAddA(HANDLE *,HMODULE,void *,DWORD,DWORD);
API DWORD WINAPI acmDriverAddW(HANDLE *,HMODULE,void *,DWORD,DWORD);
API DWORD WINAPI acmDriverOpen(HANDLE *,HANDLE,DWORD);
API DWORD WINAPI acmDriverClose(HANDLE,DWORD);
API DWORD WINAPI acmDriverRemove(HANDLE,DWORD);
API DWORD WINAPI acmDriverID(HANDLE,HANDLE *,DWORD);
API DWORD WINAPI acmDriverEnum(BOOL (WINAPI *)(HANDLE,DWORD,DWORD),DWORD,DWORD);
API DWORD WINAPI acmDriverDetailsA(HANDLE,void *,DWORD);
API DWORD WINAPI acmDriverDetailsW(HANDLE,void *,DWORD);
API HANDLE WINAPI GetDriverModuleHandle(HANDLE);
API LRESULT WINAPI DefDriverProc(DWORD,HANDLE,UINT,LPARAM,LPARAM);
API DWORD WINAPI acmFormatSuggest(HANDLE,const void *,void *,DWORD,DWORD);
API DWORD WINAPI acmStreamOpen(HANDLE *,HANDLE,const void *,const void *,void *,DWORD,DWORD,DWORD);
API DWORD WINAPI acmStreamSize(HANDLE,DWORD,DWORD *,DWORD);
API DWORD WINAPI acmStreamPrepareHeader(HANDLE,void *,DWORD);
API DWORD WINAPI acmStreamConvert(HANDLE,void *,DWORD);
API DWORD WINAPI acmStreamUnprepareHeader(HANDLE,void *,DWORD);
API DWORD WINAPI acmStreamReset(HANDLE,DWORD);
API DWORD WINAPI acmStreamClose(HANDLE,DWORD);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
#pragma pack(push,2)
typedef struct {WORD tag,channels;DWORD rate,average;WORD block,bits,extra;} FORMAT;
#pragma pack(pop)
typedef struct {DWORD size,status,user;BYTE *src;DWORD src_len,src_used,src_user;
    BYTE *dst;DWORD dst_len,dst_used,dst_user,reserved[10];} HEADER;
typedef struct {DWORD size;FORMAT *src,*dst;void *filter;DWORD callback,instance,flags,driver_flags,private_data;HANDLE stream;} STREAM;
static DWORD results[256],count,found,local,active;
static HANDLE current;
void *__cdecl memset(void *p,int c,unsigned int size) {volatile BYTE *b=p;while(size--)*b++=(BYTE)c;return p;}
static void record(DWORD v){results[count++]=v;}
static void check(BOOL v){record(v!=0);if(!v)ExitProcess(count);}
static LRESULT WINAPI driver(DWORD private_data,HANDLE handle,UINT message,LPARAM a,LPARAM b) {
    DWORD *d=(DWORD *)a;STREAM *s=(STREAM *)a;
    if(message==3)return 123;
    if(message==0x600a){DWORD size=d[0];memset(d,0,size);d[0]=size;d[1]=0x63647561;d[4]=0x04000000;d[5]=0x01000000;d[6]=1;d[7]=1;return 0;}
    if(message==0x6019){d[2]=0x7ff0;d[3]=18;d[4]=1;d[5]=0;return 0;}
    if(message==0x601b){FORMAT *src=(FORMAT *)d[2],*dst=(FORMAT *)d[4];
        if(src->tag!=0x7ff0||d[5]<18)return 512;
        dst->tag=1;dst->channels=1;dst->rate=11025;dst->average=22050;dst->block=2;dst->bits=16;dst->extra=0;return 0;}
    if(message==0x604c){if(s->src->tag!=0x7ff0||s->dst->tag!=1)return 512;
        if(!(s->flags&1)){s->private_data=77;active++;}return 0;}
    if(message==0x604d){if(s->private_data!=77)return 5;active--;return 0;}
    if(message==0x604e){DWORD *size=(DWORD *)b;if(size[1]==0)size[3]=size[2]*2;else size[2]=(size[3]+1)/2;return 0;}
    if(message==0x604f){HEADER *h=(HEADER *)b;DWORD i,n=h->src_len;
        if(n>h->dst_len/2)n=h->dst_len/2;
        if(s->private_data!=77)return 5;
        for(i=0;i<n;i++){int v=((int)h->src[i]-128)*256;h->dst[i*2]=(BYTE)v;h->dst[i*2+1]=(BYTE)(v>>8);}
        h->src_used=n;h->dst_used=n*2;return 0;}
    if(message==0x6050)return 0;
    if(message==0x6051||message==0x6052)return 8; /* Optional synchronous callbacks. */
    return message>=1&&message<=6?1:8;
}
static BOOL WINAPI enumerate(HANDLE id,DWORD context,DWORD support){if(id==current){found++;local=(support&0x40000000)!=0;}return 1;}
void mainCRTStartup(void) {
    DWORD pass,i,result,written;HANDLE id,instance,stream,identity,file;DWORD details[450];
    FORMAT src={0x7ff0,1,11025,11025,1,8,0},dst={1,1,11025,22050,2,16,0},suggested;
    BYTE input[16],output[32];HEADER header;
    for(pass=0;pass<2;pass++){
        id=instance=stream=identity=0;found=local=0;
        result=pass?acmDriverAddW(&id,GetModuleHandleA(0),(void *)driver,0,3):acmDriverAddA(&id,GetModuleHandleA(0),(void *)driver,0,3);
        record(result);check(result==0&&id);current=id;
        record(acmDriverEnum(enumerate,0,0));check(found==1&&local);
        memset(details,0,sizeof(details));details[0]=920;record(acmDriverDetailsA(id,details,0));check(details[1]==0x63647561&&details[7]==1);
        memset(details,0,sizeof(details));details[0]=1800;record(acmDriverDetailsW(id,details,0));check(details[1]==0x63647561&&details[7]==1);
        details[0]=4;record(acmDriverDetailsA(id,details,0));record(details[0]);
        details[0]=40;record(acmDriverDetailsW(id,details,0));record(details[0]);
        details[0]=1801;record(acmDriverDetailsW(id,details,0));record(details[0]);
        result=acmDriverOpen(&instance,id,0);record(result);check(result==0&&instance);
        record(acmDriverID(instance,&identity,0));check(identity==id);
        memset(&suggested,0,sizeof(suggested));record(acmFormatSuggest(instance,&src,&suggested,sizeof(suggested),0));check(suggested.tag==1&&suggested.bits==16);
        record(acmStreamOpen(0,instance,&src,&dst,0,0,0,1|4));check(active==0);
        result=acmStreamOpen(&stream,instance,&src,&dst,0,0,0,4);record(result);check(result==0&&stream&&active==1);
        record(acmDriverID(stream,&identity,0));check(identity==id);
        result=0;record(acmStreamSize(stream,16,&result,0));record(result);check(result==32);
        memset(&header,0,sizeof(header));header.size=sizeof(header);header.src=input;header.src_len=16;header.dst=output;header.dst_len=32;
        for(i=0;i<16;i++)input[i]=(BYTE)(i*17);
        record(acmStreamPrepareHeader(stream,&header,0));check((header.status&0x20000)!=0);
        record(acmStreamConvert(stream,&header,16|32));record(header.src_used);record(header.dst_used);check((header.status&0x30000)==0x30000);
        for(i=0;i<16;i++)check(output[i*2]==0&&output[i*2+1]==(BYTE)(input[i]-128));
        record(acmStreamReset(stream,0));record(acmStreamUnprepareHeader(stream,&header,0));check((header.status&0x20000)==0);
        record(acmStreamClose(stream,0));check(active==0);
        record(acmDriverClose(instance,0));record(acmDriverRemove(id,0));
    }
    for(i=1;i<=10;i++)record(DefDriverProc(0,0,i,0,0));
    file=CreateFileA("acm-function.bin",0x40000000,0,0,2,0,0);if(file==0xffffffff)ExitProcess(251);
    if(!WriteFile(file,results,count*4,&written,0)||written!=count*4)ExitProcess(252);CloseHandle(file);ExitProcess(0);
}
