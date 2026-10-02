/* DirectSound x86 COM ABI shared by the guest fixture and native-only probe. */
#ifndef ADV_FIXTURE_DSOUND_MIN_H
#define ADV_FIXTURE_DSOUND_MIN_H
#include "win32_min.h"
typedef LONG HRESULT;
#pragma pack(push, 2)
typedef struct {
    WORD tag, channels;
    DWORD rate, bytes_per_second;
    WORD align, bits, extra;
} WAVEFORMATEX;
#pragma pack(pop)
typedef struct {
    DWORD size, flags, bytes, reserved;
    WAVEFORMATEX *format;
    BYTE algorithm[16];
} DSBUFFERDESC;
API HRESULT WINAPI DirectSoundCreate(const void *, void **, void *);
typedef DWORD (WINAPI *DsRelease)(void *);
typedef HRESULT (WINAPI *DsCreateBuffer)(void *, const DSBUFFERDESC *, void **, void *);
typedef HRESULT (WINAPI *DsCooperate)(void *, HWND, DWORD);
typedef HRESULT (WINAPI *DsPosition)(void *, DWORD *, DWORD *);
typedef HRESULT (WINAPI *DsStatus)(void *, DWORD *);
typedef HRESULT (WINAPI *DsLock)(void *, DWORD, DWORD, void **, DWORD *, void **, DWORD *, DWORD);
typedef HRESULT (WINAPI *DsPlay)(void *, DWORD, DWORD, DWORD);
typedef HRESULT (WINAPI *DsStop)(void *);
typedef HRESULT (WINAPI *DsUnlock)(void *, void *, DWORD, void *, DWORD);
#define DS_METHOD(Type, object, slot) ((Type)(*(void ***)(object))[slot])
#endif
