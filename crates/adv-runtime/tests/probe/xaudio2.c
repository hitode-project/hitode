/* Authored XAudio2 graph, buffer, callback and fallback checks. No windows.
 * Build: i686-w64-mingw32-gcc -O2 -Wall -DVERSION=7 xaudio2.c -o xaudio27.exe -lole32 -luuid
 * Build: i686-w64-mingw32-gcc -O2 -Wall -DVERSION=8 xaudio2.c -o xaudio28.exe -lole32 -luuid
 * Build: i686-w64-mingw32-gcc -O2 -Wall -DVERSION=9 xaudio2.c -o xaudio29.exe -lole32 -luuid */
#define COBJMACROS
#include <windows.h>
#include <xaudio2.h>
#include <xaudio2fx.h>
#include <xapo.h>
#include <stdio.h>
#include <math.h>

static const GUID class7 = {0x5a508685,0xa254,0x4fba,{0x9b,0x82,0x9a,0x24,0xb0,0x03,0x06,0xaf}};
static const GUID iid7 = {0x8bcf1f58,0x9fe7,0x4583,{0x8a,0xc6,0xe2,0xad,0xc4,0x65,0xc8,0xbb}};
static const GUID debug7 = {0xdb05ea35,0x0329,0x4d4b,{0xa5,0x3a,0x6d,0xea,0xd0,0x3d,0x38,0x52}};
static const GUID meter7 = {0xcac1105f,0x619b,0x4d04,{0x83,0x1a,0x44,0xe1,0xcb,0xf1,0x2d,0x57}};
static const GUID echo = {0x5039d740,0xf736,0x449a,{0x84,0xd3,0xa5,0x62,0x02,0x55,0x7b,0x87}};
static HANDLE completed;
static volatile unsigned engine_depth, engine_starts, engine_ends;
static volatile unsigned starts, ends, streams, loops, errors, pass_depth, thread_id;
static unsigned contexts[32], sequence[64], count;
static volatile LONG hold_callback;
static void check(const char *name, BOOL ok) { printf("%s=%s\n",name,ok ? "ok":"FAIL"); fflush(stdout); if (!ok) ExitProcess(1); }
static void callback_thread(void) { DWORD id=GetCurrentThreadId(); if (!thread_id) thread_id=id; if (id!=thread_id) ++errors; }
static void WINAPI pass_start(IXAudio2VoiceCallback *cb, UINT32 required) { (void)cb;(void)required; callback_thread(); ++pass_depth; if (engine_depth!=1) ++errors; if (InterlockedExchange(&hold_callback,0)) Sleep(300); }
static void WINAPI pass_end(IXAudio2VoiceCallback *cb) { (void)cb; callback_thread(); if (!pass_depth) ++errors; else --pass_depth; }
static void WINAPI stream_end(IXAudio2VoiceCallback *cb) { (void)cb; callback_thread(); ++streams; if (count<64) sequence[count++]=3; SetEvent(completed); }
static void WINAPI buffer_start(IXAudio2VoiceCallback *cb,void *context) { (void)cb;callback_thread();if (!pass_depth) ++errors;++starts;if (count<64) sequence[count++]=1; (void)context; }
static void WINAPI buffer_end(IXAudio2VoiceCallback *cb,void *context) { (void)cb;callback_thread();if (!pass_depth) ++errors;if (ends<32) contexts[ends]=(unsigned)(ULONG_PTR)context;++ends;if(count<64)sequence[count++]=2; }
static void WINAPI loop_end(IXAudio2VoiceCallback *cb,void *context) { (void)cb;(void)context;callback_thread();++loops; }
static void WINAPI voice_error(IXAudio2VoiceCallback *cb,void *context,HRESULT hr) { (void)cb;(void)context;(void)hr;++errors; }
static IXAudio2VoiceCallbackVtbl callbacks = {pass_start,pass_end,stream_end,buffer_start,buffer_end,loop_end,voice_error};
static IXAudio2VoiceCallback callback = {&callbacks};
static void WINAPI engine_start(IXAudio2EngineCallback *cb) { (void)cb;callback_thread();if(engine_depth)++errors;++engine_depth;++engine_starts; }
static void WINAPI engine_end(IXAudio2EngineCallback *cb) { (void)cb;callback_thread();if(engine_depth!=1)++errors;else --engine_depth;++engine_ends; }
static void WINAPI engine_error(IXAudio2EngineCallback *cb,HRESULT hr) {(void)cb;(void)hr;++errors;}
static IXAudio2EngineCallbackVtbl engine_methods={engine_start,engine_end,engine_error};
static IXAudio2EngineCallback engine_callback={&engine_methods};
/* A foreign XAPO whose sample transform proves that Wine ran the fallback. */
static const GUID xapo_iid={0xa410b984,0x9839,0x4819,{0xa0,0xbe,0x28,0x56,0xae,0x6b,0x3a,0xdb}};
static const GUID foreign_class={0x12345678,0x4321,0x5678,{0x90,0xab,0xcd,0xef,1,2,3,4}};
static ULONG foreign_refs=1;
static UINT32 foreign_channels;
static volatile UINT32 foreign_frames;
static HRESULT WINAPI fx_query(IXAPO *self,REFIID iid,void **out) {
    *out=NULL;if(IsEqualGUID(iid,&IID_IUnknown)||IsEqualGUID(iid,&xapo_iid)) {*out=self;++foreign_refs;return S_OK;}return E_NOINTERFACE;
}
static ULONG WINAPI fx_addref(IXAPO *self) {(void)self;return ++foreign_refs;}
static ULONG WINAPI fx_release(IXAPO *self) {(void)self;return --foreign_refs;}
static HRESULT WINAPI fx_properties(IXAPO *self,XAPO_REGISTRATION_PROPERTIES **out) {
    XAPO_REGISTRATION_PROPERTIES *p=CoTaskMemAlloc(sizeof(*p));(void)self;ZeroMemory(p,sizeof(*p));p->clsid=foreign_class;
    p->Flags=XAPO_FLAG_CHANNELS_MUST_MATCH|XAPO_FLAG_FRAMERATE_MUST_MATCH|XAPO_FLAG_BITSPERSAMPLE_MUST_MATCH|XAPO_FLAG_BUFFERCOUNT_MUST_MATCH|XAPO_FLAG_INPLACE_SUPPORTED|XAPO_FLAG_INPLACE_REQUIRED;
    p->MinInputBufferCount=p->MaxInputBufferCount=p->MinOutputBufferCount=p->MaxOutputBufferCount=1;*out=p;return S_OK;
}
static HRESULT WINAPI fx_format(IXAPO *self,const WAVEFORMATEX *a,const WAVEFORMATEX *b,WAVEFORMATEX **out) {(void)self;(void)a;(void)b;if(out)*out=NULL;return S_OK;}
static HRESULT WINAPI fx_initialize(IXAPO *self,const void *data,UINT32 size) {(void)self;(void)data;(void)size;return S_OK;}
static void WINAPI fx_reset(IXAPO *self) {(void)self;}
static HRESULT WINAPI fx_lock(IXAPO *self,UINT32 in_count,const XAPO_LOCKFORPROCESS_BUFFER_PARAMETERS *in,UINT32 out_count,const XAPO_LOCKFORPROCESS_BUFFER_PARAMETERS *out) {(void)self;(void)in_count;(void)out_count;(void)out;foreign_channels=in->pFormat->nChannels;return S_OK;}
static void WINAPI fx_unlock(IXAPO *self) {(void)self;}
static void WINAPI fx_process(IXAPO *self,UINT32 in_count,const XAPO_PROCESS_BUFFER_PARAMETERS *in,UINT32 out_count,XAPO_PROCESS_BUFFER_PARAMETERS *out,BOOL enabled) {
    UINT32 i;(void)self;(void)in_count;(void)out_count;
    for(i=0;i<in->ValidFrameCount*foreign_channels;i++) ((float *)out->pBuffer)[i]=((float *)in->pBuffer)[i]*(enabled?0.5f:1.0f);
    out->ValidFrameCount=in->ValidFrameCount;out->BufferFlags=in->BufferFlags;foreign_frames+=in->ValidFrameCount;
}
static UINT32 WINAPI fx_frames(IXAPO *self,UINT32 frames) {(void)self;return frames;}
static IXAPOVtbl foreign_methods={fx_query,fx_addref,fx_release,fx_properties,fx_format,fx_format,fx_initialize,fx_reset,fx_lock,fx_unlock,fx_process,fx_frames,fx_frames};
static IXAPO foreign_effect={&foreign_methods};
static unsigned slot(unsigned n) { return VERSION==7 && n>=3 ? n+3:n; }
static void *entry(void *object,unsigned n) { return (*(void ***)object)[n]; }
static HRESULT source(void *engine,IXAudio2SourceVoice **out,WAVEFORMATEX *format,IXAudio2VoiceCallback *cb,XAUDIO2_VOICE_SENDS *sends,XAUDIO2_EFFECT_CHAIN *effects) {
    return ((HRESULT (WINAPI *)(void *,IXAudio2SourceVoice **,WAVEFORMATEX *,UINT32,float,IXAudio2VoiceCallback *,XAUDIO2_VOICE_SENDS *,XAUDIO2_EFFECT_CHAIN *))entry(engine,slot(5)))(engine,out,format,0,2.0f,cb,sends,effects);
}
static void state(IXAudio2SourceVoice *voice,XAUDIO2_VOICE_STATE *out) {
    if (VERSION==7) ((void (WINAPI *)(void *,XAUDIO2_VOICE_STATE *))entry(voice,25))(voice,out);
    else IXAudio2SourceVoice_GetState(voice,out,0);
}
static void reset_callbacks(void) { starts=ends=streams=loops=errors=pass_depth=thread_id=count=0; ResetEvent(completed); }
static void play(void *engine,IXAudio2SubmixVoice *submix,BOOL floating) {
    static short pcm[4410]; static float floats[4410];
    IXAudio2SourceVoice *voice;
    XAUDIO2_SEND_DESCRIPTOR send={0,(IXAudio2Voice *)submix};
    XAUDIO2_VOICE_SENDS sends={1,&send};
    WAVEFORMATEX format={floating?WAVE_FORMAT_IEEE_FLOAT:WAVE_FORMAT_PCM,1,44100,0,0,floating?32:16,0};
    XAUDIO2_BUFFER buffer={0}; XAUDIO2_VOICE_STATE current;
    unsigned i;
    for(i=0;i<4410;i++) {pcm[i]=(i/50)%2 ? -8192:8192;floats[i]=(i/25)%2 ? -0.5f:0.5f;}
    format.nBlockAlign=format.wBitsPerSample/8;format.nAvgBytesPerSec=44100*format.nBlockAlign;
    reset_callbacks();
    check("source",source(engine,&voice,&format,&callback,&sends,NULL)==S_OK);
    check("source-volume",IXAudio2SourceVoice_SetVolume(voice,0.5f,17)==S_OK);
    {float volume=0;IXAudio2SourceVoice_GetVolume(voice,&volume);check("volume-deferred",volume==1.0f);}
    check("commit",((HRESULT (WINAPI *)(void *,UINT32))entry(engine,slot(10)))(engine,17)==S_OK);
    {float volume=0;IXAudio2SourceVoice_GetVolume(voice,&volume);check("volume-committed",volume==0.5f);}
    check("ratio-set",IXAudio2SourceVoice_SetFrequencyRatio(voice,1.0f,0)==S_OK);
    {float ratio=0;IXAudio2SourceVoice_GetFrequencyRatio(voice,&ratio);check("ratio-get",ratio==1.0f);}
    buffer.AudioBytes=4410*format.nBlockAlign;buffer.pAudioData=floating?(BYTE *)floats:(BYTE *)pcm;buffer.pContext=(void *)0x1234;
    check("submit-first",IXAudio2SourceVoice_SubmitSourceBuffer(voice,&buffer,NULL)==S_OK);
    buffer.Flags=XAUDIO2_END_OF_STREAM;buffer.pContext=(void *)0x5678;
    check("submit-last",IXAudio2SourceVoice_SubmitSourceBuffer(voice,&buffer,NULL)==S_OK);
    if (floating) hold_callback=1;
    check("start",IXAudio2SourceVoice_Start(voice,0,0)==S_OK);
    {XAUDIO2_PERFORMANCE_DATA performance;((void (WINAPI *)(void *,XAUDIO2_PERFORMANCE_DATA *))entry(engine,slot(11)))(engine,&performance);check("performance",performance.ActiveSourceVoiceCount==1 && performance.TotalSourceVoiceCount==1 && performance.ActiveSubmixVoiceCount==1 && performance.CurrentLatencyInSamples>0);}
    if (floating) {Sleep(250);state(voice,&current);check("late-callback-consumption",current.BuffersQueued==0);}
    check("done",WaitForSingleObject(completed,5000)==WAIT_OBJECT_0);
    state(voice,&current);
    check("state-empty",current.BuffersQueued==0);
    check("contexts",ends==2 && contexts[0]==0x1234 && contexts[1]==0x5678);
    printf("callbacks starts=%u ends=%u streams=%u count=%u errors=%u depth=%u\n",starts,ends,streams,count,errors,pass_depth);fflush(stdout);
    check("callback-order",starts==2 && streams==1 && count==5 && sequence[0]==1 && sequence[1]==2 && sequence[2]==1 && sequence[3]==2 && sequence[4]==3 && !errors);
    IXAudio2SourceVoice_DestroyVoice(voice);
}
int main(void) {
    HMODULE dll;void *engine=NULL;IXAudio2MasteringVoice *master;IXAudio2SubmixVoice *submix;IClassFactory *factory;
    IUnknown *meter;IXAudio2SourceVoice *voice;
    WAVEFORMATEX format={WAVE_FORMAT_IEEE_FLOAT,1,44100,176400,4,32,0};
    static float samples[22050];XAUDIO2_BUFFER buffer={0};XAUDIO2_VOICE_STATE current;
    XAUDIO2_EFFECT_DESCRIPTOR effect;XAUDIO2_EFFECT_CHAIN chain={1,&effect};
    float peak[1]={0},rms[1]={0};struct { float *peak, *rms; UINT32 channels; } levels={peak,rms,1};
    unsigned i;HRESULT hr;
    CoInitializeEx(NULL,COINIT_MULTITHREADED);completed=CreateEventA(NULL,FALSE,FALSE,NULL);
    dll=LoadLibraryA(VERSION==7?"xaudio2_7.dll":VERSION==8?"xaudio2_8.dll":"xaudio2_9.dll");check("module",dll!=NULL);
    if (VERSION==7) {
        void *debug_engine=NULL;
        check("debug-factory",((HRESULT (WINAPI *)(const GUID *,const GUID *,void **))GetProcAddress(dll,"DllGetClassObject"))(&debug7,&IID_IClassFactory,(void **)&factory)==S_OK);
        IClassFactory_Release(factory);check("debug-engine",CoCreateInstance(&debug7,NULL,CLSCTX_INPROC_SERVER,&iid7,&debug_engine)==S_OK);((ULONG (WINAPI *)(void *))entry(debug_engine,2))(debug_engine);
        check("factory",((HRESULT (WINAPI *)(const GUID *,const GUID *,void **))GetProcAddress(dll,"DllGetClassObject"))(&class7,&IID_IClassFactory,(void **)&factory)==S_OK);
        IClassFactory_Release(factory);
        check("engine-com",CoCreateInstance(&class7,NULL,CLSCTX_INPROC_SERVER,&iid7,&engine)==S_OK);
        check("initialize",((HRESULT (WINAPI *)(void *,UINT32,UINT32))entry(engine,5))(engine,0,0xffffffff)==S_OK);
    } else check("engine-create",((HRESULT (WINAPI *)(void **,UINT32,UINT32))GetProcAddress(dll,"XAudio2Create"))(&engine,0,0xffffffff)==S_OK);
    check("engine-callbacks",((HRESULT (WINAPI *)(void *,IXAudio2EngineCallback *))entry(engine,slot(3)))(engine,&engine_callback)==S_OK);
    if (VERSION==7) hr=((HRESULT (WINAPI *)(void *,IXAudio2MasteringVoice **,UINT32,UINT32,UINT32,UINT32,void *))entry(engine,slot(7)))(engine,&master,2,44100,0,0,NULL);
    else hr=((HRESULT (WINAPI *)(void *,IXAudio2MasteringVoice **,UINT32,UINT32,UINT32,void *,void *,UINT32))entry(engine,slot(7)))(engine,&master,2,44100,0,NULL,NULL,6);
    check("master",hr==S_OK);
    check("submix",((HRESULT (WINAPI *)(void *,IXAudio2SubmixVoice **,UINT32,UINT32,UINT32,UINT32,void *,void *))entry(engine,slot(6)))(engine,&submix,2,44100,0,0,NULL,NULL)==S_OK);
    play(engine,submix,FALSE);play(engine,submix,TRUE);
    check("engine-passes",engine_starts>0 && engine_ends<=engine_starts);
    ((void (WINAPI *)(void *,IXAudio2EngineCallback *))entry(engine,slot(4)))(engine,&engine_callback);
    if (VERSION==7) {
        check("meter-factory",((HRESULT (WINAPI *)(const GUID *,const GUID *,void **))GetProcAddress(dll,"DllGetClassObject"))(&meter7,&IID_IClassFactory,(void **)&factory)==S_OK);
        check("meter",IClassFactory_CreateInstance(factory,NULL,&IID_IUnknown,(void **)&meter)==S_OK);IClassFactory_Release(factory);
    } else check("meter",((HRESULT (WINAPI *)(IUnknown **))GetProcAddress(dll,"CreateAudioVolumeMeter"))(&meter)==S_OK);
    effect.pEffect=meter;effect.InitialState=TRUE;effect.OutputChannels=1;
    check("meter-source",source(engine,&voice,&format,NULL,NULL,&chain)==S_OK);
    check("effect-disable",IXAudio2SourceVoice_DisableEffect(voice,0,0)==S_OK);
    {BOOL enabled=TRUE;IXAudio2SourceVoice_GetEffectState(voice,0,&enabled);check("effect-disabled",!enabled);}
    check("effect-enable-deferred",IXAudio2SourceVoice_EnableEffect(voice,0,17)==S_OK);
    {BOOL enabled=TRUE;IXAudio2SourceVoice_GetEffectState(voice,0,&enabled);check("effect-still-disabled",!enabled);}
    check("effect-commit",((HRESULT (WINAPI *)(void *,UINT32))entry(engine,slot(10)))(engine,17)==S_OK);
    {BOOL enabled=FALSE;IXAudio2SourceVoice_GetEffectState(voice,0,&enabled);check("effect-enabled",enabled);}
    for(i=0;i<22050;i++) samples[i]=0.375f;
    buffer.AudioBytes=sizeof(samples);buffer.pAudioData=(BYTE *)samples;buffer.Flags=XAUDIO2_END_OF_STREAM;buffer.pContext=(void *)0xabcd;
    check("meter-submit",IXAudio2SourceVoice_SubmitSourceBuffer(voice,&buffer,NULL)==S_OK);
    check("meter-start",IXAudio2SourceVoice_Start(voice,0,0)==S_OK);Sleep(50);
    check("meter-get",IXAudio2SourceVoice_GetEffectParameters(voice,0,&levels,sizeof(levels))==S_OK);
    printf("peak=%.6f rms=%.6f\n",peak[0],rms[0]);fflush(stdout);
    check("played-peak",fabsf(peak[0]-0.375f)<0.001f);
    /* The callback worker and the caller both wait while the device consumes. */
    Sleep(600);state(voice,&current);check("blocked-completion",current.BuffersQueued==0);
    IXAudio2SourceVoice_DestroyVoice(voice);IUnknown_Release(meter);
    if (VERSION==9) {
        IUnknown *fx;IXAudio2SourceVoice *queued;
        check("queued-source",source(engine,&queued,&format,NULL,NULL,NULL)==S_OK);
        check("queued-submit",IXAudio2SourceVoice_SubmitSourceBuffer(queued,&buffer,NULL)==S_OK);
        check("foreign-effect",((HRESULT (__cdecl *)(const GUID *,IUnknown **,void *,UINT32))GetProcAddress(dll,"CreateFX"))(&echo,&fx,NULL,0)==S_OK);
        IUnknown_Release(fx);
        effect.pEffect=(IUnknown *)&foreign_effect;effect.InitialState=TRUE;effect.OutputChannels=1;
        check("fallback-source",source(engine,&voice,&format,NULL,NULL,&chain)==S_OK);
        check("fallback-submit",IXAudio2SourceVoice_SubmitSourceBuffer(voice,&buffer,NULL)==S_OK);
        check("queued-start",IXAudio2SourceVoice_Start(queued,0,0)==S_OK);
        check("fallback-start",IXAudio2SourceVoice_Start(voice,0,0)==S_OK);Sleep(700);state(voice,&current);
        check("fallback-completion",current.BuffersQueued==0);state(queued,&current);check("queued-completion",current.BuffersQueued==0);
        check("foreign-processed",foreign_frames>1000);IXAudio2SourceVoice_DestroyVoice(voice);IXAudio2SourceVoice_DestroyVoice(queued);
    }
    IXAudio2SubmixVoice_DestroyVoice(submix);IXAudio2MasteringVoice_DestroyVoice(master);
    ((ULONG (WINAPI *)(void *))entry(engine,2))(engine);CloseHandle(completed);CoUninitialize();
    puts("xaudio ok");return 0;
}
