/* Exercise Wine's waveOut and DirectSound render paths through mmdevapi.
 * Build: i686-w64-mingw32-gcc -O1 -mcrtdll=msvcrt-os -o audio.exe audio.c -lwinmm -ldsound -ldxguid -ladvapi32 */
#define COBJMACROS
#include <windows.h>
#include <mmsystem.h>
#include <dsound.h>
#include <stdio.h>

#define FRAMES 8820
static short wave[FRAMES * 2];
static short sound[FRAMES * 2];

int main(void)
{
    HKEY key;
    HWAVEOUT output;
    WAVEFORMATEX format = {WAVE_FORMAT_PCM, 2, 44100, 176400, 4, 16, 0};
    WAVEHDR header = {0};
    IDirectSound *ds;
    IDirectSoundBuffer *buffer;
    DSBUFFERDESC desc = {0};
    void *first, *second;
    DWORD first_size, second_size, start, before, after, write;
    MMRESULT mm;
    HRESULT hr;
    unsigned int i;

    setvbuf(stdout, NULL, _IONBF, 0);
    if (RegCreateKeyExW(HKEY_CURRENT_USER, L"Software\\Wine\\Drivers", 0, NULL,
                        0, KEY_SET_VALUE, NULL, &key, NULL)) return 1;
    if (RegSetValueExW(key, L"Audio", 0, REG_SZ, (const BYTE *)L"advaudio",
                       sizeof(L"advaudio"))) return 2;
    RegCloseKey(key);
    printf("midi_out=%u midi_in=%u aux=%u\n", midiOutGetNumDevs(), midiInGetNumDevs(), auxGetNumDevs());
    printf("render=%u capture=%u\n", waveOutGetNumDevs(), waveInGetNumDevs());
    for (i = 0; i < FRAMES; ++i)
    {
        wave[i * 2] = wave[i * 2 + 1] = (i % 100 < 50) ? 8192 : -8192;
        sound[i * 2] = sound[i * 2 + 1] = (i % 50 < 25) ? 16384 : -16384;
    }
    mm = waveOutOpen(&output, WAVE_MAPPER, &format, 0, 0, CALLBACK_NULL);
    if (mm) { printf("wave_open=%u\n", mm); return 3; }
    header.lpData = (char *)wave;
    header.dwBufferLength = sizeof(wave);
    if (waveOutPrepareHeader(output, &header, sizeof(header))) return 4;
    if (waveOutWrite(output, &header, sizeof(header))) return 5;
    start = GetTickCount();
    while (!(header.dwFlags & WHDR_DONE) && GetTickCount() - start < 5000) Sleep(5);
    printf("wave_done=%u bytes=%lu\n", !!(header.dwFlags & WHDR_DONE), header.dwBufferLength);
    if (!(header.dwFlags & WHDR_DONE)) return 6;
    if (waveOutUnprepareHeader(output, &header, sizeof(header))) return 7;
    if (waveOutClose(output)) return 8;

    hr = DirectSoundCreate(NULL, &ds, NULL);
    if (FAILED(hr)) { printf("ds_create=%08lx\n", hr); return 9; }
    hr = IDirectSound_SetCooperativeLevel(ds, GetDesktopWindow(), DSSCL_NORMAL);
    if (FAILED(hr)) return 10;
    desc.dwSize = sizeof(desc);
    desc.dwFlags = DSBCAPS_GETCURRENTPOSITION2 | DSBCAPS_GLOBALFOCUS;
    desc.dwBufferBytes = sizeof(sound);
    desc.lpwfxFormat = &format;
    hr = IDirectSound_CreateSoundBuffer(ds, &desc, &buffer, NULL);
    if (FAILED(hr)) { printf("ds_buffer=%08lx\n", hr); return 11; }
    if (FAILED(IDirectSoundBuffer_Lock(buffer, 0, sizeof(sound), &first, &first_size,
                                       &second, &second_size, 0))) return 12;
    memcpy(first, sound, first_size);
    if (second_size) memcpy(second, (BYTE *)sound + first_size, second_size);
    if (FAILED(IDirectSoundBuffer_Unlock(buffer, first, first_size, second, second_size))) return 13;
    if (FAILED(IDirectSoundBuffer_GetCurrentPosition(buffer, &before, &write))) return 14;
    if (FAILED(IDirectSoundBuffer_Play(buffer, 0, 0, DSBPLAY_LOOPING))) return 15;
    Sleep(150);
    if (FAILED(IDirectSoundBuffer_GetCurrentPosition(buffer, &after, &write))) return 16;
    printf("ds_advanced=%u before=%lu after=%lu write=%lu\n", after != before, before, after, write);
    IDirectSoundBuffer_Stop(buffer);
    IDirectSoundBuffer_Release(buffer);
    IDirectSound_Release(ds);
    return after == before ? 17 : 0;
}
