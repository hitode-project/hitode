/* Repository-owned Vorbis callback ABI fixture. The two Ogg inputs are generated
 * sine tones; no game data, CRT, visible window or audio device is required. */
#include "win32_min.h"
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI ReadFile(HANDLE,void *,DWORD,DWORD *,void *);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
API DWORD WINAPI SetFilePointer(HANDLE,LONG,LONG *,DWORD);
API DWORD WINAPI GetFileSize(HANDLE,DWORD *);
typedef unsigned int SIZE_T;
typedef long long I64;
typedef struct {
    SIZE_T (__cdecl *read)(void *,SIZE_T,SIZE_T,void *);
    int (__cdecl *seek)(void *,I64,int);
    int (__cdecl *close)(void *);
    long (__cdecl *tell)(void *);
} CALLBACKS;
typedef union { double aligned; BYTE raw[720]; } VF;
typedef struct { int version,channels; long rate,upper,nominal,lower,window; void *setup; } INFO;
typedef struct { char **comments; int *lengths,count; char *vendor; } COMMENT;
typedef int (__cdecl *OPEN)(void *,VF *,const char *,long,CALLBACKS);
typedef long (__cdecl *READ)(VF *,char *,int,int,int,int,int *);
typedef struct { HANDLE file; DWORD position,length,closed,short_read,fail_read,fail_seek;
    VF *nested_vf; READ nested_read; DWORD nested_calls; } SOURCE;
typedef int (__cdecl *CLEAR)(VF *);
typedef int (__cdecl *TEST_OPEN)(VF *);
typedef INFO *(__cdecl *GET_INFO)(VF *,int);
typedef COMMENT *(__cdecl *GET_COMMENT)(VF *,int);
typedef long (__cdecl *LINK_VALUE)(VF *,int);
typedef long (__cdecl *VF_VALUE)(VF *);
typedef double (__cdecl *TIME_TOTAL)(VF *,int);
typedef double (__cdecl *TIME_TELL)(VF *);
typedef I64 (__cdecl *PCM_TOTAL)(VF *,int);
typedef I64 (__cdecl *PCM_TELL)(VF *);
typedef int (__cdecl *PCM_SEEK)(VF *,I64);
typedef int (__cdecl *TIME_SEEK)(VF *,double);
static DWORD test;
int _fltused=0;
static void check(BOOL value) { ++test; if (!value) ExitProcess(test); }
static SIZE_T __cdecl read_cb(void *out,SIZE_T size,SIZE_T count,void *datasource) {
    SOURCE *s=(SOURCE *)datasource;
    DWORD bytes=size*count,read=0;
    if (s->fail_read) return 0;
    if (s->short_read && bytes>s->short_read) bytes=s->short_read;
    if (!ReadFile(s->file,out,bytes,&read,NULL)) return 0xffffffffu;
    s->position+=read;
    if (s->nested_vf) {
        char nested[4]; int section;
        check(s->nested_read(s->nested_vf,nested,4,0,2,1,&section)==-131);
        ++s->nested_calls;
    }
    Sleep(0); /* A guest callback can park without losing its caller's state. */
    return size ? read/size : 0;
}
static int __cdecl seek_cb(void *datasource,I64 offset,int whence) {
    SOURCE *s=(SOURCE *)datasource; I64 target=offset;
    if (s->fail_seek) return -1;
    if (whence==1) target+=s->position;
    if (whence==2) target+=s->length;
    if (target<0 || target>s->length) return -1;
    if (SetFilePointer(s->file,(LONG)target,NULL,0)==0xffffffffu) return -1;
    s->position=(DWORD)target; return 0;
}
static int __cdecl close_cb(void *datasource) { ++((SOURCE *)datasource)->closed; return 0; }
static long __cdecl tell_cb(void *datasource) { return (long)((SOURCE *)datasource)->position; }
static void reset(SOURCE *s) { s->position=0; SetFilePointer(s->file,0,NULL,0); }
void mainCRTStartup(void) {
    SOURCE source={0},other={0}; static VF vf,second; CALLBACKS callbacks={read_cb,seek_cb,close_cb,tell_cb};
    HMODULE dll=LoadLibraryA("vorbisfile.dll");
    OPEN open=(OPEN)GetProcAddress(dll,"ov_open_callbacks");
    OPEN probe=(OPEN)GetProcAddress(dll,"ov_test_callbacks");
    READ read=(READ)GetProcAddress(dll,"ov_read");
    CLEAR clear=(CLEAR)GetProcAddress(dll,"ov_clear");
    TEST_OPEN test_open=(TEST_OPEN)GetProcAddress(dll,"ov_test_open");
    GET_INFO info=(GET_INFO)GetProcAddress(dll,"ov_info");
    GET_COMMENT comment=(GET_COMMENT)GetProcAddress(dll,"ov_comment");
    VF_VALUE streams=(VF_VALUE)GetProcAddress(dll,"ov_streams");
    VF_VALUE seekable=(VF_VALUE)GetProcAddress(dll,"ov_seekable");
    LINK_VALUE serial=(LINK_VALUE)GetProcAddress(dll,"ov_serialnumber");
    TIME_TOTAL time=(TIME_TOTAL)GetProcAddress(dll,"ov_time_total");
    PCM_TOTAL total=(PCM_TOTAL)GetProcAddress(dll,"ov_pcm_total");
    PCM_TELL tell=(PCM_TELL)GetProcAddress(dll,"ov_pcm_tell");
    PCM_TELL raw_tell=(PCM_TELL)GetProcAddress(dll,"ov_raw_tell");
    PCM_SEEK seek=(PCM_SEEK)GetProcAddress(dll,"ov_pcm_seek");
    TIME_TELL time_tell=(TIME_TELL)GetProcAddress(dll,"ov_time_tell");
    TIME_SEEK time_seek=(TIME_SEEK)GetProcAddress(dll,"ov_time_seek");
    char bytes[4096],initial[11]; DWORD n,written,decoded=0,position; int section;
    HANDLE output; long result; INFO *vi; COMMENT *vc; BOOL standard; I64 frame_total;
    check(dll && open && probe && read && clear && test_open && info && comment && streams && seekable && serial && time && time_tell && total && tell && raw_tell && seek);
    source.file=CreateFileA("fixture_codec.ogg",0x80000000u,1,NULL,3,0,0);
    check(source.file!=0xffffffffu);
    source.length=GetFileSize(source.file,NULL); source.short_read=17;
#ifdef DECODE_ONLY
    /* Generic full-stream decoder for offline source/reference comparisons. */
    source.short_read=0;
    check(open(&source,&vf,NULL,0,callbacks)==0);
    vi=info(&vf,-1); check(vi && vi->channels>0 && vi->rate>0);
    {
        DWORD metadata[4]; I64 frames=total(&vf,-1); frame_total=frames;
        metadata[0]=(DWORD)vi->channels; metadata[1]=(DWORD)vi->rate;
        metadata[2]=(DWORD)frames; metadata[3]=(DWORD)(frames>>32);
        output=CreateFileA("metadata.bin",0x40000000u,0,NULL,2,0,0); check(output!=0xffffffffu);
        check(WriteFile(output,metadata,16,&written,NULL) && written==16); CloseHandle(output);
    }
    output=CreateFileA("output.pcm",0x40000000u,0,NULL,2,0,0); check(output!=0xffffffffu);
    while ((result=read(&vf,bytes,4096,0,2,1,&section))>0) {
        check(WriteFile(output,bytes,(DWORD)result,&written,NULL) && written==(DWORD)result);
        decoded+=(DWORD)result;
    }
    check(result==0 && decoded>0); CloseHandle(output);
    check(frame_total>257 && seek(&vf,frame_total-257)==0);
    output=CreateFileA("tail.pcm",0x40000000u,0,NULL,2,0,0); check(output!=0xffffffffu);
    while ((result=read(&vf,bytes,4096,0,2,1,&section))>0) {
        check(WriteFile(output,bytes,(DWORD)result,&written,NULL) && written==(DWORD)result);
    }
    check(result==0); CloseHandle(output);
    check(seek(&vf,0)==0 && tell(&vf)==0);
    result=read(&vf,bytes,4096,0,2,1,&section); check(result>0);
    output=CreateFileA("loop.pcm",0x40000000u,0,NULL,2,0,0); check(output!=0xffffffffu);
    check(WriteFile(output,bytes,(DWORD)result,&written,NULL) && written==(DWORD)result); CloseHandle(output);
    check(clear(&vf)==0 && source.closed==1); CloseHandle(source.file);
    ExitProcess(0);
#endif
    check(probe(&source,&vf,NULL,0,callbacks)==0);
    standard=(*(DWORD *)(vf.raw+0x58)==1);
    if (standard) {
        check(tell(&vf)==-131 && raw_tell(&vf)==-131 && time_tell(&vf)==-131.0);
        check(read(&vf,bytes,4096,0,2,1,&section)==-131 && seek(&vf,0)==-131);
    }
    check(clear(&vf)==0); check(source.closed==1);
    check(info(&vf,-1)==NULL && comment(&vf,-1)==NULL && streams(&vf)==0 && seekable(&vf)==0);
    reset(&source);
    check(probe(&source,&vf,NULL,0,callbacks)==0);
    source.short_read=0;
    /* Some original 1.0 DLLs leave STREAMSET here; their own ov_test_open
     * rejects that state. The standard PARTOPEN transition is checked when
     * the provider implements it, including our native host provider. */
    if (*(DWORD *)(vf.raw+0x58)==1) check(test_open(&vf)==0);
    check(clear(&vf)==0); check(source.closed==2);
    reset(&source); source.short_read=0;
    check(ReadFile(source.file,initial,11,&n,NULL) && n==11); source.position=11;
    check(open(&source,&vf,initial,11,callbacks)==0);
    vi=info(&vf,-1); check(vi!=NULL);
#ifdef STEREO_FIXTURE
    check(vi->channels==2);
#else
    check(vi->channels==1);
#endif
    check(vi->rate==44100);
    check(*(INFO **)(vf.raw+0x48)==vi); /* The standard x86 public prefix. */
    vc=comment(&vf,-1); check(vc && vc->vendor && vc->vendor[0]);
    check(vc->count>=1 && vc->comments && vc->lengths && vc->lengths[0]>0);
    check(vc->comments[vc->count]==NULL && vc->comments[0][vc->lengths[0]]==0);
    check(streams(&vf)==1 && seekable(&vf)==1);
    check((DWORD)serial(&vf,-1)==*(DWORD *)(vf.raw+0x5c));
    position=source.position;
    check(time(&vf,-1)>0.249 && time(&vf,-1)<0.251);
    check(source.position==position); /* Metadata may seek, but restores read state. */
    check(total(&vf,-1)==11025);
    if (standard) {
        I64 *lengths=*(I64 **)(vf.raw+0x44),*offsets=*(I64 **)(vf.raw+0x38);
        check(lengths[0]==0 && lengths[1]==11025);
        check(offsets[0]==0 && offsets[1]==source.length);
        check(raw_tell(&vf)==*(I64 *)(vf.raw+8));
    }
    check(seek(&vf,2205)==0); check(tell(&vf)==2205);
    check(time_seek && time_seek(&vf,0.05)==0 && tell(&vf)==2205);
    check(time_tell(&vf)>0.04999 && time_tell(&vf)<0.05001);
    result=read(&vf,bytes,4096,0,2,1,&section); check(result>0 && section==0);
    output=CreateFileA("seek.pcm",0x40000000u,0,NULL,2,0,0); check(output!=0xffffffffu);
    check(WriteFile(output,bytes,(DWORD)result,&written,NULL) && written==(DWORD)result); CloseHandle(output);
    {
        char reference[32],unsigned8[32]; DWORD i;
        check(seek(&vf,500)==0); check(read(&vf,reference,32,0,2,1,&section)==32);
        check(seek(&vf,500)==0); check(read(&vf,bytes,32,1,2,1,&section)==32);
        for (i=0;i<32;i+=2) check(bytes[i]==reference[i+1] && bytes[i+1]==reference[i]);
        check(seek(&vf,500)==0); check(read(&vf,bytes,32,0,2,0,&section)==32);
        for (i=0;i<32;i+=2) check(*(unsigned short *)(bytes+i)==(unsigned short)(*(unsigned short *)(reference+i)+32768u));
        check(seek(&vf,500)==0); check(read(&vf,bytes,16,0,1,1,&section)==16);
        check(seek(&vf,500)==0); check(read(&vf,unsigned8,16,0,1,0,&section)==16);
        for (i=0;i<16;++i) {
            int expected=((int)*(short *)(reference+i*2)+128)>>8;
            int actual=(int)(signed char)bytes[i];
            check(actual>=expected-1 && actual<=expected+1);
            check((BYTE)unsigned8[i]==(BYTE)(bytes[i]+128));
        }
    }
    check(seek(&vf,0)==0); check(tell(&vf)==0);
    other.file=CreateFileA("fixture_codec.ogg",0x80000000u,1,NULL,3,0,0);
    check(other.file!=0xffffffffu); other.length=GetFileSize(other.file,NULL);
    check(open(&other,&second,NULL,0,callbacks)==0);
    check(read(&second,bytes,4096,0,2,1,&section)>0); check(tell(&second)>0);
    check(tell(&vf)==0); check(clear(&second)==0 && other.closed==1 && source.closed==2);
    CloseHandle(other.file);
    output=CreateFileA("output.pcm",0x40000000u,0,NULL,2,0,0); check(output!=0xffffffffu);
    while ((result=read(&vf,bytes,4096,0,2,1,&section))>0) {
        check(WriteFile(output,bytes,(DWORD)result,&written,NULL) && written==(DWORD)result);
        decoded+=(DWORD)result;
    }
    check(result==0);
    /* Old libvorbis 1.0 exposes final packet padding after a seek. The host
     * provider clips to the EOS granule; SNR uses only the 11025 valid frames. */
    check(decoded>=11025u*2u*(DWORD)vi->channels && decoded<=(11025u+8192u)*2u*(DWORD)vi->channels);
    check(tell(&vf)==decoded/(2u*(DWORD)vi->channels)); CloseHandle(output);
    check(clear(&vf)==0); check(source.closed==3);
    reset(&source); source.fail_seek=1; /* Streaming callbacks report ENOSEEK. */
    check(open(&source,&vf,NULL,0,callbacks)==0);
    check(time(&vf,-1)==-131.0); check(seek(&vf,0)==-138);
    check(clear(&vf)==0); check(source.closed==4);
    reset(&source); source.fail_seek=0; source.fail_read=1;
    check(open(&source,&vf,NULL,0,callbacks)<0); check(source.closed==4);
    if (standard) {
        /* NULL seek/tell/close is legal for streaming sources. Some legacy
         * DLLs crash on these inputs, so their ABI baseline skips this part. */
        reset(&source); source.fail_read=0;
        callbacks.seek=NULL; callbacks.tell=NULL; callbacks.close=NULL;
        check(open(&source,&vf,NULL,0,callbacks)==0);
        check(seekable(&vf)==0 && seek(&vf,0)==-138);
        check(clear(&vf)==0 && source.closed==4);
        callbacks.seek=seek_cb; callbacks.tell=tell_cb; callbacks.close=close_cb;
        reset(&source); check(open(&source,&vf,NULL,0,callbacks)==0);
        source.fail_seek=1; check(seek(&vf,0)==-128); source.fail_seek=0;
        source.nested_vf=&vf; source.nested_read=read;
        check(seek(&vf,0)==0);
        check(read(&vf,bytes,4096,0,2,1,&section)>0 && source.nested_calls>0);
        source.nested_vf=NULL;
        check(clear(&vf)==0 && source.closed==5);
        check(clear(&vf)==0 && source.closed==5);
    }
    CloseHandle(source.file);
    ExitProcess(0);
}
