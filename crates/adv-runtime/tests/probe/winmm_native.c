/* Authored native WinMM queue, callbacks, formats and named-sound checks.
 * Build: i686-w64-mingw32-gcc -O2 -Wall winmm_native.c -o winmm_native.exe -lwinmm -ladvapi32 */
#include <windows.h>
#include <mmsystem.h>
#include <mmreg.h>
#include <stdio.h>
#include <string.h>

static WAVEHDR headers[3];
static unsigned opened, closed, done;
static HANDLE completed;
static DWORD main_thread, callback_thread;
static short samples[3][4410 * 2];
static void check(const char *name, BOOL ok)
{
    printf("%s=%s\n", name, ok ? "ok" : "FAIL");
    fflush(stdout);
    if (!ok) ExitProcess(1);
}
static void CALLBACK callback(HWAVEOUT wave, UINT msg, DWORD_PTR instance,
                             DWORD_PTR first, DWORD_PTR second)
{
    (void)wave; (void)second;
    check("callback-instance", instance == 0x1234);
    if (msg == WOM_OPEN) ++opened;
    if (msg == WOM_CLOSE) ++closed;
    if (msg == WOM_DONE) {
        check("callback-order", (WAVEHDR *)first == &headers[done]);
        callback_thread = GetCurrentThreadId();
        ++done;
        if (done == 3) SetEvent(completed);
    }
}
static void queue(HWAVEOUT output)
{
    unsigned i;
    for (i = 0; i < 3; ++i) {
        memset(&headers[i], 0, sizeof(headers[i]));
        headers[i].lpData = (char *)samples[i];
        headers[i].dwBufferLength = sizeof(samples[i]);
        check("prepare", !waveOutPrepareHeader(output, &headers[i], sizeof(headers[i])));
        check("write", !waveOutWrite(output, &headers[i], sizeof(headers[i])));
    }
    check("busy-close", waveOutClose(output) == WAVERR_STILLPLAYING);
    check("busy-unprepare", waveOutUnprepareHeader(output, &headers[0], sizeof(headers[0])) == WAVERR_STILLPLAYING);
}
static void release(HWAVEOUT output)
{
    unsigned i;
    for (i = 0; i < 3; ++i) {
        check("header-done", (headers[i].dwFlags & WHDR_DONE) && !(headers[i].dwFlags & WHDR_INQUEUE));
        check("unprepare", !waveOutUnprepareHeader(output, &headers[i], sizeof(headers[i])));
    }
    check("close", !waveOutClose(output));
}
static LRESULT CALLBACK window_proc(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message == WOM_DONE) {
        check("window-order", (WAVEHDR *)lparam == &headers[done]);
        ++done;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}
static void callback_kinds(void)
{
    WAVEFORMATEX format = {WAVE_FORMAT_PCM, 2, 44100, 176400, 4, 16, 0};
    HWAVEOUT output;
    MSG msg;
    unsigned i, j;
    WNDCLASSW cls = {0};
    HWND window;
    cls.lpfnWndProc = window_proc;
    cls.lpszClassName = L"NativeAudioProbe";
    cls.hInstance = GetModuleHandleW(NULL);
    check("class", RegisterClassW(&cls));
    window = CreateWindowW(cls.lpszClassName, L"", 0, 0, 0, 0, 0, HWND_MESSAGE, NULL, cls.hInstance, NULL);
    check("message-window", window != NULL);
    for (i = 0; i < 3; ++i)
        for (j = 0; j < 4410; ++j)
            samples[i][2*j] = samples[i][2*j+1] = (j % 100 < 50 ? 1 : -1) * (4096 * (i+1));
    main_thread = GetCurrentThreadId();
    completed = CreateEventW(NULL, FALSE, FALSE, NULL);
    check("function-open", !waveOutOpen(&output, 0, &format, (DWORD_PTR)callback, 0x1234, CALLBACK_FUNCTION));
    check("open-order", opened == 1);
    queue(output);
    check("blocked-completion", WaitForSingleObject(completed, 3000) == WAIT_OBJECT_0 && done == 3);
    check("worker-thread", callback_thread != main_thread);
    release(output);
    check("close-order", closed == 1);
    done = 0;
    check("window-open", !waveOutOpen(&output, WAVE_MAPPER, &format, (DWORD_PTR)window, 0, CALLBACK_WINDOW));
    queue(output);
    while (done < 3 && GetMessageW(&msg, NULL, 0, 0) > 0) DispatchMessageW(&msg);
    release(output);
    done = 0;
    PeekMessageW(&msg, NULL, 0, 0, PM_NOREMOVE);
    check("thread-open", !waveOutOpen(&output, 0, &format, main_thread, 0, CALLBACK_THREAD));
    queue(output);
    while (done < 3 && GetMessageW(&msg, NULL, 0, 0) > 0) {
        if (msg.message == WOM_DONE) {
            check("thread-order", (WAVEHDR *)msg.lParam == &headers[done]);
            ++done;
        } else DispatchMessageW(&msg);
    }
    release(output);
    done = 0;
    check("event-open", !waveOutOpen(&output, 0, &format, (DWORD_PTR)completed, 0, CALLBACK_EVENT));
    ResetEvent(completed);
    queue(output);
    while (!(headers[2].dwFlags & WHDR_DONE))
        check("event-completion", WaitForSingleObject(completed, 3000) == WAIT_OBJECT_0);
    release(output);
    check("bad-window", waveOutOpen(&output,0,&format,(DWORD_PTR)0x123456,0,CALLBACK_WINDOW)==MMSYSERR_INVALPARAM);
    DestroyWindow(window);
}
static void controls(void)
{
    WAVEFORMATEX format = {WAVE_FORMAT_PCM, 2, 44100, 176400, 4, 16, 0};
    HWAVEOUT output;
    MMTIME before = {TIME_SAMPLES}, after = {TIME_SAMPLES};
    DWORD volume;
    UINT device, mixer;
    WAVEOUTCAPSW caps;
    unsigned i;
    const UINT time_types[] = {TIME_MS, TIME_BYTES, TIME_SMPTE, TIME_MIDI, TIME_TICKS};
    done=0;
    check("reset-callback-open", !waveOutOpen(&output,0,&format,(DWORD_PTR)callback,0x1234,CALLBACK_FUNCTION));
    queue(output);
    check("reset-callback", !waveOutReset(output));
    release(output);
    check("reset-close-order", done==3 && closed==2);
    check("control-open", !waveOutOpen(&output, 0, &format, 0, 0, 0));
    check("native-id", !waveOutGetID(output,&device) && device==0);
    check("native-caps", !waveOutGetDevCapsW((UINT_PTR)output,&caps,sizeof(caps)) && caps.wChannels==2);
    check("native-mixer", !mixerGetID((HMIXEROBJ)output,&mixer,MIXER_OBJECTF_HWAVEOUT) && mixer==0);
    check("pitch", waveOutSetPitch(output, 0x10000) == MMSYSERR_NOTSUPPORTED);
    check("rate", waveOutSetPlaybackRate(output, 0x10000) == MMSYSERR_NOTSUPPORTED);
    check("volume", !waveOutSetVolume(output, 0x7fff3fff) && !waveOutGetVolume(output, &volume) && volume == 0x7fff3fff);
    check("pause-empty", !waveOutPause(output));
    queue(output);
    Sleep(60);
    check("paused-queued", !(headers[0].dwFlags & WHDR_DONE));
    check("restart", !waveOutRestart(output));
    Sleep(40);
    check("pause", !waveOutPause(output));
    check("position", !waveOutGetPosition(output, &before, sizeof(before)) && before.u.sample > 0);
    Sleep(60);
    check("frozen-position", !waveOutGetPosition(output, &after, sizeof(after)) && after.u.sample == before.u.sample);
    for(i=0;i<sizeof(time_types)/sizeof(time_types[0]);++i) {
        after.wType=time_types[i];
        check("position-format", !waveOutGetPosition(output,&after,sizeof(after)) && after.wType==TIME_BYTES && after.u.cb==before.u.sample*4);
    }
    check("reset", !waveOutReset(output));
    release(output);
    check("loop-open", !waveOutOpen(&output, 0, &format, 0, 0, 0));
    memset(&headers[0], 0, sizeof(headers[0]));
    headers[0].lpData = (char *)samples[0];
    headers[0].dwBufferLength = sizeof(samples[0]);
    headers[0].dwFlags = WHDR_BEGINLOOP | WHDR_ENDLOOP;
    headers[0].dwLoops = 100;
    check("loop-prepare", !waveOutPrepareHeader(output, &headers[0], sizeof(headers[0])));
    check("loop-write", !waveOutWrite(output, &headers[0], sizeof(headers[0])));
    Sleep(250);
    check("loop-active", !(headers[0].dwFlags & WHDR_DONE));
    check("break-loop", !waveOutBreakLoop(output));
    Sleep(200);
    check("loop-done", headers[0].dwFlags & WHDR_DONE);
    check("loop-unprepare", !waveOutUnprepareHeader(output, &headers[0], sizeof(headers[0])));
    check("loop-close", !waveOutClose(output));
}
static void named_sound(void)
{
    struct { char riff[4]; DWORD size; char wave[4]; char fmt[4]; DWORD fmt_size;
             WAVEFORMATEX format; WORD padding; char data[4]; DWORD data_size; short pcm[8820]; } wav;
    HMMIO mmio;
    HANDLE file;
    DWORD written, start, error;
    unsigned i;
    char status[80];
    WCHAR executable[MAX_PATH];
    HANDLE update;
    HMODULE resource;
    HKEY key;
    ZeroMemory(&wav, sizeof(wav));
    memcpy(wav.riff,"RIFF",4); wav.size=sizeof(wav)-8; memcpy(wav.wave,"WAVE",4);
    memcpy(wav.fmt,"fmt ",4); wav.fmt_size=sizeof(WAVEFORMATEX)+sizeof(WORD);
    wav.format.wFormatTag=WAVE_FORMAT_PCM; wav.format.nChannels=2;
    wav.format.nSamplesPerSec=44100; wav.format.nAvgBytesPerSec=176400;
    wav.format.nBlockAlign=4; wav.format.wBitsPerSample=16;
    memcpy(wav.data,"data",4); wav.data_size=sizeof(wav.pcm);
    for (i=0;i<8820;++i) wav.pcm[i]=(i/2%100<50)?8192:-8192;
    start=GetTickCount();
    check("memory-sync", PlaySoundW((LPCWSTR)&wav,NULL,SND_MEMORY|SND_SYNC));
    check("sync-wait", GetTickCount()-start >= 80);
    check("memory-async", PlaySoundA((LPCSTR)&wav,NULL,SND_MEMORY|SND_ASYNC|SND_LOOP));
    check("nostop", !PlaySoundA((LPCSTR)&wav,NULL,SND_MEMORY|SND_ASYNC|SND_NOSTOP));
    Sleep(240);
    check("stop", PlaySoundW(NULL,NULL,0));
    file=CreateFileW(L"native-probe.wav",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    check("wave-file", file!=INVALID_HANDLE_VALUE && WriteFile(file,&wav,sizeof(wav),&written,NULL) && written==sizeof(wav));
    CloseHandle(file);
    mmio=mmioOpenW(L"native-probe.wav",NULL,MMIO_READ);
    check("forwarded-mmio", mmio && !mmioClose(mmio,0));
    check("named-file", sndPlaySoundW(L"native-probe.wav",SND_SYNC|SND_NODEFAULT));
    check("search-extension", PlaySoundW(L"native-probe", NULL, SND_FILENAME | SND_SYNC));
    check("missing-alias", !PlaySoundW(L"NativeMissingSound",NULL,SND_ALIAS|SND_NODEFAULT|SND_SYNC));
    check("alias-key", !RegCreateKeyExW(HKEY_CURRENT_USER,L"AppEvents\\Schemes\\Apps\\.Default\\NativeProbe\\.Default",0,NULL,0,KEY_SET_VALUE,NULL,&key,NULL));
    check("alias-value", !RegSetValueExW(key,NULL,0,REG_SZ,(const BYTE *)L"native-probe.wav",sizeof(L"native-probe.wav")));
    RegCloseKey(key);
    check("registry-alias", PlaySoundW(L"NativeProbe",NULL,SND_ALIAS|SND_NODEFAULT|SND_SYNC));
    check("profile-write", WriteProfileStringW(L"Sounds",L"NativeProfile",L"native-probe.wav,probe"));
    check("profile-alias", PlaySoundW(L"NativeProfile",NULL,SND_ALIAS|SND_NODEFAULT|SND_SYNC));
    WriteProfileStringW(L"Sounds",L"NativeProfile",NULL);
    GetModuleFileNameW(NULL,executable,MAX_PATH);
    check("resource-copy", CopyFileW(executable,L"native-resource.dll",FALSE));
    update=BeginUpdateResourceW(L"native-resource.dll",FALSE);
    check("resource-update", update && UpdateResourceW(update,L"WAVE",MAKEINTRESOURCEW(7),0,&wav,sizeof(wav)) && UpdateResourceW(update,L"WAVE",L"ProbeWave",0,&wav,sizeof(wav)) && EndUpdateResourceW(update,FALSE));
    resource=LoadLibraryExW(L"native-resource.dll",NULL,LOAD_LIBRARY_AS_DATAFILE);
    check("resource-load", resource!=NULL);
    check("resource-id", PlaySoundW(MAKEINTRESOURCEW(7),resource,SND_RESOURCE|SND_SYNC));
    check("resource-name", PlaySoundA("ProbeWave",resource,SND_RESOURCE|SND_SYNC));
    FreeLibrary(resource);
    DeleteFileW(L"native-resource.dll");
    {
        struct { char riff[4]; DWORD size; char wave[4]; char fmt[4]; DWORD fmt_size;
                 WAVEFORMATEX format; WORD padding; char data[4]; DWORD data_size; BYTE pcm[800]; } compressed;
        ZeroMemory(&compressed,sizeof(compressed));
        memcpy(compressed.riff,"RIFF",4);compressed.size=sizeof(compressed)-8;
        memcpy(compressed.wave,"WAVE",4);memcpy(compressed.fmt,"fmt ",4);
        compressed.fmt_size=sizeof(WAVEFORMATEX)+sizeof(WORD);
        compressed.format.wFormatTag=WAVE_FORMAT_MULAW;
        compressed.format.nChannels=1;compressed.format.nSamplesPerSec=8000;
        compressed.format.nAvgBytesPerSec=8000;compressed.format.nBlockAlign=1;
        compressed.format.wBitsPerSample=8;
        memcpy(compressed.data,"data",4);compressed.data_size=sizeof(compressed.pcm);
        memset(compressed.pcm,0x80,sizeof(compressed.pcm));
        check("memory-acm", PlaySoundW((LPCWSTR)&compressed,NULL,SND_MEMORY|SND_SYNC));
    }
    check("mci-wave-open", !mciSendStringA("open native-probe.wav type waveaudio alias sample",NULL,0,NULL));
    error=mciSendStringA("play sample wait",NULL,0,NULL);
    printf("mci-wave-result=%lu\n",error);
    check("mci-wave-play", !error);
    check("mci-wave-status", !mciSendStringA("status sample mode",status,sizeof(status),NULL) && !lstrcmpA(status,"stopped"));
    check("mci-wave-close", !mciSendStringA("close sample",NULL,0,NULL));
    DeleteFileW(L"native-probe.wav");
}
static void formats(void)
{
    WAVEFORMATEXTENSIBLE format;
    HWAVEOUT output;
    WAVEHDR header;
    BYTE pcm[4096];
    unsigned i;
    const WORD bits[] = {8,16,24,32,32,16,24,32,32};
    const GUID pcm_guid={1,0,0x10,{0x80,0,0,0xaa,0,0x38,0x9b,0x71}};
    ZeroMemory(pcm,sizeof(pcm));
    for(i=0;i<sizeof(bits)/sizeof(bits[0]);++i) {
        ZeroMemory(&format,sizeof(format));
        format.Format.wFormatTag=i<4?WAVE_FORMAT_PCM:(i==4?WAVE_FORMAT_IEEE_FLOAT:WAVE_FORMAT_EXTENSIBLE);
        format.Format.nChannels=2;
        format.Format.nSamplesPerSec=44100;
        format.Format.wBitsPerSample=bits[i];
        format.Format.nBlockAlign=2*bits[i]/8;
        format.Format.nAvgBytesPerSec=44100*format.Format.nBlockAlign;
        if(i>=5) {
            format.Format.cbSize=22;
            format.Samples.wValidBitsPerSample=bits[i];
            format.dwChannelMask=3;
            format.SubFormat=pcm_guid;
            if(i==8) format.SubFormat.Data1=3;
        }
        check("format-query", !waveOutOpen(NULL,0,&format.Format,0,0,WAVE_FORMAT_QUERY));
        check("format-open", !waveOutOpen(&output,0,&format.Format,0,0,0));
        ZeroMemory(&header,sizeof(header));
        header.lpData=(char *)pcm;
        header.dwBufferLength=441*format.Format.nBlockAlign;
        check("format-prepare", !waveOutPrepareHeader(output,&header,sizeof(header)));
        check("format-write", !waveOutWrite(output,&header,sizeof(header)));
        Sleep(40);
        check("format-done", header.dwFlags&WHDR_DONE);
        check("format-unprepare", !waveOutUnprepareHeader(output,&header,sizeof(header)));
        check("format-close", !waveOutClose(output));
    }
}
int main(void)
{
    callback_kinds();
    controls();
    formats();
    named_sound();
    CloseHandle(completed);
    puts("native winmm ok");
    return 0;
}
