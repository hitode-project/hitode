/* Authored x86 ACM ABI and codec differential fixture. Native msacm32 is only
 * a reference provider; the guest imports are implemented with Symphonia. */
#include "win32_min.h"
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
API void *WINAPI VirtualAlloc(void *,DWORD,DWORD,DWORD);
API BOOL WINAPI VirtualProtect(void *,DWORD,DWORD,DWORD *);
#pragma pack(push,2)
typedef struct { WORD tag,channels; DWORD rate,average; WORD block,bits,extra; } FORMAT;
typedef struct { FORMAT base; WORD frames; } IMA_FORMAT;
typedef struct { short a,b; } COEFFICIENT;
typedef struct { FORMAT base; WORD frames,count; COEFFICIENT coeff[7]; } MS_FORMAT;
#pragma pack(pop)
typedef struct { DWORD size,status,user; BYTE *src; DWORD src_length,src_used,src_user;
    BYTE *dst; DWORD dst_length,dst_used,dst_user,reserved[10]; } HEADER;
typedef char HeaderSizeMustBe84[(sizeof(HEADER)==84)?1:-1];
typedef char FormatSizeMustBe18[(sizeof(FORMAT)==18)?1:-1];
API DWORD WINAPI acmFormatSuggest(HANDLE,const FORMAT *,FORMAT *,DWORD,DWORD);
API DWORD WINAPI acmStreamOpen(HANDLE *,HANDLE,const FORMAT *,const FORMAT *,const void *,DWORD,DWORD,DWORD);
API DWORD WINAPI acmStreamSize(HANDLE,DWORD,DWORD *,DWORD);
API DWORD WINAPI acmStreamPrepareHeader(HANDLE,HEADER *,DWORD);
API DWORD WINAPI acmStreamConvert(HANDLE,HEADER *,DWORD);
API DWORD WINAPI acmStreamUnprepareHeader(HANDLE,HEADER *,DWORD);
API DWORD WINAPI acmStreamClose(HANDLE,DWORD);
static DWORD results[512],count,test;
static BYTE source[2048],output[8192];
static HEADER header;
static void record(DWORD value) { results[count++]=value; }
static void save(const char *name,const void *bytes,DWORD length) {
    DWORD written; HANDLE file=CreateFileA(name,0x40000000u,0,NULL,2,0,0);
    if(file==0xffffffffu || !WriteFile(file,bytes,length,&written,NULL) || written!=length) ExitProcess(250);
    CloseHandle(file);
}
static void check(BOOL value) { ++test; if(!value) { save("acm-results.bin",results,count*4); ExitProcess(test); } }
static void zero(void *bytes,DWORD length) { volatile BYTE *p=(volatile BYTE *)bytes; while(length--) *p++=0; }
void *__cdecl memset(void *bytes,int value,unsigned int length) {
    volatile BYTE *p=(volatile BYTE *)bytes;while(length--)*p++=(BYTE)value;return bytes;
}
static void put16(BYTE *p,int value) { p[0]=(BYTE)value; p[1]=(BYTE)(value>>8); }
static void format_pcm(FORMAT *f,WORD channels,WORD bits) {
    zero(f,sizeof(*f)); f->tag=1;f->channels=channels;f->rate=11025;
    f->block=channels*(bits/8);f->average=f->rate*f->block;f->bits=bits;
}
static void run(DWORD id,FORMAT *src,FORMAT *dst) {
    HANDLE stream=0; FORMAT suggested; DWORD value,result,block=src->block;
    DWORD decoded=(src->tag==1?1:*(WORD *)((BYTE *)src+18))*dst->block;
    DWORD source_length=block*(src->tag==1?128:2),destination_length=decoded*(src->tag==1?128:2);
    const char *names[6]={"acm-pcm8.pcm","acm-pcm16.pcm","acm-ima-mono.pcm",
        "acm-ima-stereo.pcm","acm-ms-mono.pcm","acm-ms-stereo.pcm"};
    zero(&suggested,sizeof(suggested)); suggested.tag=1;suggested.channels=dst->channels;
    suggested.rate=dst->rate;suggested.bits=dst->bits;
    record(id);result=acmFormatSuggest(0,src,&suggested,sizeof(suggested),0xf0000);record(result);check(result==0);
    record(suggested.tag);record(suggested.channels);record(suggested.rate);record(suggested.average);
    record(suggested.block);record(suggested.bits);record(suggested.extra);
    result=acmStreamOpen(NULL,0,src,dst,NULL,0,0,1|4);record(result);check(result==0);
    result=acmStreamOpen(&stream,0,src,dst,NULL,0,0,4);record(result);check(result==0 && stream);
    value=0;result=acmStreamSize(stream,source_length,&value,0);record(result);record(value);check(result==0 && value>=destination_length);
    value=0;result=acmStreamSize(stream,destination_length,&value,1);record(result);record(value);check(result==0 && value>=source_length);
    zero(&header,sizeof(header));header.size=sizeof(header);header.src=source;header.src_length=source_length;
    header.dst=output;header.dst_length=destination_length;header.user=0x12345678;header.src_user=0xaabbccdd;header.dst_user=0x88776655;
    result=acmStreamConvert(stream,&header,4|16|32);record(result);check(result==514);
    result=acmStreamPrepareHeader(stream,&header,0);record(result);record(header.status);check(result==0);
    result=acmStreamPrepareHeader(stream,&header,0);record(result);check(result==0);
    zero(output,sizeof(output));
    result=acmStreamConvert(stream,&header,4|16|32);record(result);record(header.status);record(header.src_used);record(header.dst_used);
    check(result==0 && header.src_used==source_length && header.dst_used==destination_length);
    check(header.user==0x12345678 && header.src_user==0xaabbccdd && header.dst_user==0x88776655);
    save(names[id],output,header.dst_used);
    /* Source data may shrink; destination capacity stays fixed while prepared. */
    header.src_length=source_length/2;
    result=acmStreamConvert(stream,&header,4|16|32);record(result);record(header.src_used);record(header.dst_used);
    check(result==0 && header.src_used==source_length/2 && header.dst_used==destination_length/2);
    header.src_length=source_length;header.dst_length=destination_length;
    result=acmStreamUnprepareHeader(stream,&header,0);record(result);record(header.status);check(result==0);
    result=acmStreamUnprepareHeader(stream,&header,0);record(result);check(result==514);
    result=acmStreamClose(stream,0);record(result);check(result==0);
    result=acmStreamClose(stream,0);record(result);check(result==5);
}
static void ms_edges(void) {
    static const WORD deltas[9]={0,1,15,16,256,16384,32767,32768,65535};
    static const COEFFICIENT coefficients[7]={{256,0},{512,-256},{0,0},{192,64},{240,0},{460,-208},{392,-232}};
    DWORD channels,predictor,d,pattern,i,ch,written,metadata[8]; HANDLE stream,file;
    FORMAT pcm;MS_FORMAT ms;
    file=CreateFileA("acm-ms-edges.pcm",0x40000000u,0,NULL,2,0,0);check(file!=0xffffffffu);
    for(channels=1;channels<=2;++channels) {
        zero(&ms,sizeof(ms));ms.base.tag=2;ms.base.channels=(WORD)channels;
        ms.base.rate=11025;ms.base.block=(WORD)(256*channels);ms.base.bits=4;ms.base.extra=32;
        ms.frames=500;ms.count=7;ms.base.average=ms.base.rate*ms.base.block/ms.frames;
        for(i=0;i<7;++i){ms.coeff[i].a=coefficients[i].a;ms.coeff[i].b=coefficients[i].b;}
        format_pcm(&pcm,(WORD)channels,16);
        check(acmStreamOpen(&stream,0,&ms.base,&pcm,NULL,0,0,4)==0);
        zero(&header,sizeof(header));header.size=sizeof(header);header.src=source;header.src_length=ms.base.block;
        header.dst=output;header.dst_length=ms.frames*pcm.block;
        check(acmStreamPrepareHeader(stream,&header,0)==0);
        for(predictor=0;predictor<7;++predictor)for(d=0;d<9;++d)for(pattern=0;pattern<4;++pattern){
            /* Pattern zero preserves the original arbitrary byte sequence that
             * exposed the upstream debug overflow. The other patterns exercise
             * continuous maximum, opposite-sign and zero residuals. */
            for(i=0;i<ms.base.block;++i) source[i]=pattern==0?(BYTE)(i*11+3):pattern==1?0x88:pattern==2?0x77:0;
            for(ch=0;ch<channels;++ch){
                source[ch]=(BYTE)((predictor+ch)%7);put16(source+channels+2*ch,deltas[d]);
                put16(source+3*channels+2*ch,ch?-32768:32767);
                put16(source+5*channels+2*ch,ch?12345:-12345);
            }
            metadata[0]=channels;metadata[1]=predictor;metadata[2]=deltas[d];metadata[3]=pattern;
            metadata[4]=acmStreamConvert(stream,&header,4|16|32);metadata[5]=header.src_used;
            metadata[6]=header.dst_used;metadata[7]=header.status;
            check(WriteFile(file,metadata,sizeof(metadata),&written,NULL) && written==sizeof(metadata));
            check(metadata[4]==0 && header.src_used==ms.base.block && header.dst_used==header.dst_length);
            check(WriteFile(file,output,header.dst_used,&written,NULL) && written==header.dst_used);
        }
        check(acmStreamUnprepareHeader(stream,&header,0)==0);check(acmStreamClose(stream,0)==0);
    }
    CloseHandle(file);
}
static void ima_edges(void) {
    static const BYTE indices[6]={0,1,25,29,87,88};static const short predictors[4]={-32768,-1,0,32767};
    DWORD channels,index,predictor,pattern,i,ch,written,metadata[8];HANDLE stream,file;FORMAT pcm;IMA_FORMAT ima;
    file=CreateFileA("acm-ima-edges.pcm",0x40000000u,0,NULL,2,0,0);check(file!=0xffffffffu);
    for(channels=1;channels<=2;++channels){
        zero(&ima,sizeof(ima));ima.base.tag=0x11;ima.base.channels=(WORD)channels;ima.base.rate=11025;
        ima.base.block=(WORD)(256*channels);ima.base.bits=4;ima.base.extra=2;ima.frames=505;
        ima.base.average=ima.base.rate*ima.base.block/ima.frames;format_pcm(&pcm,(WORD)channels,16);
        check(acmStreamOpen(&stream,0,&ima.base,&pcm,NULL,0,0,4)==0);
        zero(&header,sizeof(header));header.size=84;header.src=source;header.src_length=ima.base.block;
        header.dst=output;header.dst_length=ima.frames*pcm.block;check(acmStreamPrepareHeader(stream,&header,0)==0);
        for(index=0;index<6;++index)for(predictor=0;predictor<4;++predictor)for(pattern=0;pattern<5;++pattern){
            for(i=0;i<ima.base.block;++i)source[i]=pattern==0?(BYTE)(i*17+5):pattern==1?0x88:pattern==2?0x77:pattern==3?0:0x13;
            for(ch=0;ch<channels;++ch){put16(source+ch*4,predictors[ch?3-predictor:predictor]);source[ch*4+2]=indices[index];source[ch*4+3]=0;}
            metadata[0]=channels;metadata[1]=indices[index];metadata[2]=(DWORD)(int)predictors[predictor];metadata[3]=pattern;
            metadata[4]=acmStreamConvert(stream,&header,4|16|32);metadata[5]=header.src_used;metadata[6]=header.dst_used;metadata[7]=header.status;
            check(WriteFile(file,metadata,sizeof(metadata),&written,NULL) && written==sizeof(metadata));
            check(metadata[4]==0 && header.src_used==ima.base.block && header.dst_used==header.dst_length);
            check(WriteFile(file,output,header.dst_used,&written,NULL) && written==header.dst_used);
        }
        check(acmStreamUnprepareHeader(stream,&header,0)==0);check(acmStreamClose(stream,0)==0);
    }
    CloseHandle(file);
}
#ifdef HOST_BOUNDARIES
static void boundaries(void) {
    FORMAT pcm8,pcm16,bad;IMA_FORMAT ima;MS_FORMAT ms;HANDLE stream=0;DWORD value,i,old,offset=0;
    BYTE *large_source,*large_output;static const COEFFICIENT coefficients[7]={{256,0},{512,-256},{0,0},{192,64},{240,0},{460,-208},{392,-232}};
    format_pcm(&pcm8,1,8);format_pcm(&pcm16,1,16);bad=pcm16;
    bad.rate=0xffffffffu;check(acmStreamOpen(&stream,0,&bad,&pcm16,NULL,0,0,4)==512);
    bad=pcm8;bad.tag=3;check(acmStreamOpen(&stream,0,&bad,&pcm16,NULL,0,0,4)==512);
    check(acmStreamOpen(&stream,0,(FORMAT *)0xfffffffc,&pcm16,NULL,0,0,4)==11);
    check(acmStreamOpen(&stream,0,&pcm8,&pcm16,NULL,0,0,8)==10);
    check(acmStreamOpen(&stream,0,&pcm8,&pcm16,NULL,0,0,2)==8);
    check(acmStreamOpen(&stream,0,&pcm8,&pcm16,NULL,1,0,4)==11);
    check(acmStreamOpen(NULL,0,&pcm8,&pcm16,NULL,0,0,4)==11);
    value=0x12345678;check(acmStreamOpen(&value,0,&pcm8,&pcm16,NULL,0,0,1|4)==0 && value==0x12345678);
    check(acmStreamOpen(&stream,0,&pcm8,&pcm16,NULL,0,0,4)==0);
    value=7;check(acmStreamSize(stream,0,&value,0)==11 && value==0);
    value=7;check(acmStreamSize(stream,1,&value,2)==10 && value==0);
    value=7;check(acmStreamSize(stream,0xffffffffu,&value,0)==512 && value==0);
    zero(&header,sizeof(header));header.size=83;header.src=source;header.src_length=128;header.dst=output;header.dst_length=256;
    check(acmStreamPrepareHeader(stream,&header,0)==11);header.size=84;
    header.dst=(BYTE *)0x3000;check(acmStreamPrepareHeader(stream,&header,0)==11);header.dst=output;
    check(acmStreamPrepareHeader(stream,&header,0)==0);
    header.dst_length=128;check(acmStreamConvert(stream,&header,4|16|32)==11);header.dst_length=256;
    header.src=source+1;check(acmStreamConvert(stream,&header,4|16|32)==11);header.src=source;
    header.src_length=129;check(acmStreamConvert(stream,&header,4|16|32)==11);header.src_length=128;
    check(acmStreamConvert(stream,&header,1)==10);
    check(acmStreamConvert(stream,&header,4|16|32)==0 && header.src_used==128 && header.dst_used==256);
    header.src_length=64;check(acmStreamUnprepareHeader(stream,&header,0)==11);header.src_length=128;
    check(acmStreamUnprepareHeader(stream,&header,0)==0);check(acmStreamClose(stream,0)==0);
    zero(&ima,sizeof(ima));ima.base.tag=0x11;ima.base.channels=1;ima.base.rate=11025;
    ima.base.block=256;ima.base.bits=4;ima.base.extra=2;ima.frames=504;
    check(acmStreamOpen(&stream,0,&ima.base,&pcm16,NULL,0,0,4)==512);ima.frames=505;
    check(acmStreamOpen(&stream,0,&ima.base,&pcm16,NULL,0,0,4)==0);
    zero(&header,sizeof(header));header.size=84;header.src=source;header.src_length=256;header.dst=output;header.dst_length=1010;
    zero(source,256);source[2]=89;for(i=0;i<1010;++i)output[i]=0x5a;
    check(acmStreamPrepareHeader(stream,&header,0)==0);
    check(acmStreamConvert(stream,&header,4|16|32)==512 && header.src_used==0 && header.dst_used==0);
    for(i=0;i<1010;++i)check(output[i]==0x5a);
    source[2]=25;header.src_length=255;
    check(acmStreamConvert(stream,&header,16|32)==512 && header.src_used==0 && header.dst_used==0);
    header.src_length=256;check(acmStreamConvert(stream,&header,4|16|32)==0 && header.dst_used==1010);
    check(acmStreamUnprepareHeader(stream,&header,0)==0);check(acmStreamClose(stream,0)==0);
    zero(&ms,sizeof(ms));ms.base.tag=2;ms.base.channels=1;ms.base.rate=11025;
    ms.base.block=256;ms.base.bits=4;ms.base.extra=32;ms.frames=500;ms.count=7;
    for(i=0;i<7;++i){ms.coeff[i].a=coefficients[i].a;ms.coeff[i].b=coefficients[i].b;}
    ms.coeff[6].a=391;check(acmStreamOpen(&stream,0,&ms.base,&pcm16,NULL,0,0,4)==512);ms.coeff[6].a=392;
    check(acmStreamOpen(&stream,0,&ms.base,&pcm16,NULL,0,0,4)==0);
    zero(&header,sizeof(header));header.size=84;header.src=source;header.src_length=256;header.dst=output;header.dst_length=1000;
    zero(source,256);source[0]=7;check(acmStreamPrepareHeader(stream,&header,0)==0);
    check(acmStreamConvert(stream,&header,4|16|32)==512 && header.src_used==0 && header.dst_used==0);
    check(acmStreamUnprepareHeader(stream,&header,0)==0);check(acmStreamClose(stream,0)==0);
    /* Four bounded calls must consume the full input with exact Used counts. */
    large_source=(BYTE *)VirtualAlloc(NULL,262144,0x3000,4);large_output=(BYTE *)VirtualAlloc(NULL,524288,0x3000,4);
    check(large_source && large_output);for(i=0;i<262144;++i)large_source[i]=(BYTE)(i*13+17);
    check(acmStreamOpen(&stream,0,&pcm8,&pcm16,NULL,0,0,4)==0);
    while(offset<262144){
        zero(&header,sizeof(header));header.size=84;header.src=large_source+offset;header.src_length=262144-offset;
        header.dst=large_output+offset*2;header.dst_length=(262144-offset)*2;
        check(acmStreamPrepareHeader(stream,&header,0)==0);
        check(acmStreamConvert(stream,&header,4|16|32)==0 && header.src_used==65536 && header.dst_used==131072);
        check(acmStreamUnprepareHeader(stream,&header,0)==0);offset+=65536;
    }
    for(i=0;i<262144;++i)check(*(short *)(large_output+i*2)==((int)large_source[i]-128)*256);
    /* Permissions are checked even though host accessors can service readonly pages. */
    zero(&header,sizeof(header));header.size=84;header.src=large_source;header.src_length=128;header.dst=large_output;header.dst_length=256;
    check(VirtualProtect(large_output,4096,2,&old));check(acmStreamPrepareHeader(stream,&header,0)==11);
    check(VirtualProtect(large_output,4096,old,&old));check(acmStreamClose(stream,0)==0);
    /* PCMWAVEFORMAT is 16 bytes; cbSize must not be read beyond its page. */
    for(i=0;i<16;++i)large_source[4096-16+i]=((BYTE *)&pcm8)[i];
    check(VirtualProtect(large_source+4096,4096,1,&old));
    check(acmStreamOpen(NULL,0,(FORMAT *)(large_source+4096-16),&pcm16,NULL,0,0,1|4)==0);
    check(VirtualProtect(large_source+4096,4096,old,&old));
    check(VirtualProtect(large_source,4096,1,&old));
    check(acmStreamOpen(NULL,0,(FORMAT *)(large_source+4096-16),&pcm16,NULL,0,0,1|4)==11);
    check(VirtualProtect(large_source,4096,old,&old));
}
#endif
void mainCRTStartup(void) {
    FORMAT pcm8,pcm16; IMA_FORMAT ima; MS_FORMAT ms; DWORD i,ch,b,channels;
    static const COEFFICIENT coefficients[7]={{256,0},{512,-256},{0,0},{192,64},{240,0},{460,-208},{392,-232}};
#ifdef HOST_BOUNDARIES
    boundaries();ExitProcess(0);
#endif
    format_pcm(&pcm8,1,8);format_pcm(&pcm16,1,16);
    for(i=0;i<sizeof(source);++i)source[i]=(BYTE)(i*13+37);
    source[0]=0;source[1]=1;source[2]=127;source[3]=128;
    run(0,&pcm8,&pcm16);
    format_pcm(&pcm16,2,16);format_pcm(&pcm8,2,8);
    put16(source,-32768);put16(source+2,-1);put16(source+4,32767);put16(source+6,0);
    run(1,&pcm16,&pcm8);
    for(channels=1;channels<=2;++channels) {
        zero(&ima,sizeof(ima));ima.base.tag=0x11;ima.base.channels=(WORD)channels;
        ima.base.rate=11025;ima.base.block=(WORD)(256*channels);ima.base.bits=4;ima.base.extra=2;
        ima.frames=505;ima.base.average=ima.base.rate*ima.base.block/ima.frames;
        for(b=0;b<2;++b) {
            BYTE *p=source+b*ima.base.block;
            for(i=0;i<ima.base.block;++i) p[i]=(BYTE)((i*17+b*31+5)&0xff);
            for(ch=0;ch<channels;++ch) { put16(p+ch*4,ch? -1234:1234);p[ch*4+2]=(BYTE)(25+ch*4);p[ch*4+3]=0; }
        }
        format_pcm(&pcm16,(WORD)channels,16);run(1+channels,&ima.base,&pcm16);
    }
    for(channels=1;channels<=2;++channels) {
        zero(&ms,sizeof(ms));ms.base.tag=2;ms.base.channels=(WORD)channels;
        ms.base.rate=11025;ms.base.block=(WORD)(256*channels);ms.base.bits=4;ms.base.extra=32;
        ms.frames=500;ms.count=7;ms.base.average=ms.base.rate*ms.base.block/ms.frames;
        for(i=0;i<7;++i) { ms.coeff[i].a=coefficients[i].a;ms.coeff[i].b=coefficients[i].b; }
        for(b=0;b<2;++b) {
            BYTE *p=source+b*ms.base.block;
            /* Small signed residuals keep the adaptive delta bounded, as an
             * ordinary encoder does for these slowly varying samples. */
            for(i=0;i<ms.base.block;++i) p[i]=(BYTE)(((i%4)<<4)|((i+1)%4));
            for(ch=0;ch<channels;++ch) {
                p[ch]=(BYTE)(ch+b);put16(p+channels+ch*2,16+ch*8);
                put16(p+channels*3+ch*2,ch?-1200:1200);put16(p+channels*5+ch*2,ch?-1100:1100);
            }
        }
        format_pcm(&pcm16,(WORD)channels,16);run(3+channels,&ms.base,&pcm16);
    }
    ms_edges();ima_edges();save("acm-results.bin",results,count*4);ExitProcess(0);
}
