/* Authored DirectMusic loader, collection, port and native dmsynth probe.
 * Build: i686-w64-mingw32-gcc -O2 -Wall dmusic.c -o dmusic.exe -lole32 -luuid */
#define COBJMACROS
#define INITGUID
#include <windows.h>
#include <objbase.h>
#include <stdio.h>
#include <string.h>

DEFINE_GUID(CLSID_DirectMusic, 0x636b9f10,0x0c7d,0x11d1,0x95,0xb2,0x00,0x20,0xaf,0xdc,0x74,0x21);
DEFINE_GUID(CLSID_DirectMusicLoader, 0xd2ac2892,0xb39b,0x11d1,0x87,0x04,0x00,0x60,0x08,0x93,0xb1,0xbd);
DEFINE_GUID(CLSID_DirectMusicCollection, 0x480ff4b0,0x28b2,0x11d1,0xbe,0xf7,0x00,0xc0,0x4f,0xbf,0x8f,0xef);
DEFINE_GUID(GUID_DefaultGMCollection, 0xf17e8673,0xc3b4,0x11d1,0x87,0x0b,0x00,0x60,0x08,0x93,0xb1,0xbd);
DEFINE_GUID(GUID_DMUS_PROP_Volume, 0xfedfae25,0xe46e,0x11d1,0xaa,0xce,0x00,0x00,0xf8,0x75,0xac,0x12);
DEFINE_GUID(IID_IDirectMusic8, 0x2d3629f7,0x813d,0x4939,0x85,0x08,0xf0,0x5c,0x6b,0x75,0xfd,0x97);
DEFINE_GUID(IID_IDirectMusicLoader8, 0x19e7c08c,0x0a44,0x4e6a,0xa1,0x16,0x59,0x5a,0x7c,0xd5,0xde,0x8c);
DEFINE_GUID(IID_IDirectMusicCollection, 0xd2ac287c,0xb39b,0x11d1,0x87,0x04,0x00,0x60,0x08,0x93,0xb1,0xbd);
DEFINE_GUID(IID_IKsControl, 0x28f54685,0x06fd,0x11d2,0xb2,0x7a,0x00,0xa0,0xc9,0x22,0x31,0x96);

typedef LONGLONG REFERENCE_TIME;

typedef struct {
    DWORD dwSize;
    DWORD dwValidParams;
    DWORD dwVoices;
    DWORD dwChannelGroups;
    DWORD dwAudioChannels;
    DWORD dwSampleRate;
    DWORD dwEffectFlags;
    BOOL  fShare;
    DWORD dwFeatures;
} DMUS_PORTPARAMS8;

typedef struct {
    DWORD dwSize;
    DWORD dwFlags;
    GUID  guidBufferFormat;
    DWORD cbBuffer;
} DMUS_BUFFERDESC;

typedef struct {
    DWORD dwSize;
    DWORD dwValidData;
    GUID  guidObject;
    GUID  guidClass;
    FILETIME ftDate;
    DWORD dwVersionMS;
    DWORD dwVersionLS;
    WCHAR wszName[64];
    WCHAR wszCategory[64];
    WCHAR wszFileName[MAX_PATH];
    LONGLONG llMemLength;
    BYTE *pbMemData;
    IStream *pStream;
} DMUS_OBJECTDESC;

typedef struct {
    GUID Set;
    ULONG Id;
    ULONG Flags;
} KSIDENTIFIER;

typedef struct IDirectMusic8 IDirectMusic8;
typedef struct IDirectMusicPort IDirectMusicPort;
typedef struct IDirectMusicBuffer IDirectMusicBuffer;
typedef struct IDirectMusicLoader8 IDirectMusicLoader8;
typedef struct IDirectMusicCollection IDirectMusicCollection;
typedef struct IDirectMusicInstrument IDirectMusicInstrument;
typedef struct IDirectMusicDownloadedInstrument IDirectMusicDownloadedInstrument;
typedef struct IReferenceClock IReferenceClock;
typedef struct IKsControl IKsControl;

typedef struct {
    HRESULT (STDMETHODCALLTYPE *QueryInterface)(IDirectMusic8*, REFIID, void**);
    ULONG   (STDMETHODCALLTYPE *AddRef)(IDirectMusic8*);
    ULONG   (STDMETHODCALLTYPE *Release)(IDirectMusic8*);
    HRESULT (STDMETHODCALLTYPE *EnumPort)(IDirectMusic8*, DWORD, void*);
    HRESULT (STDMETHODCALLTYPE *CreateMusicBuffer)(IDirectMusic8*, DMUS_BUFFERDESC*, IDirectMusicBuffer**, IUnknown*);
    HRESULT (STDMETHODCALLTYPE *CreatePort)(IDirectMusic8*, REFCLSID, DMUS_PORTPARAMS8*, IDirectMusicPort**, IUnknown*);
    HRESULT (STDMETHODCALLTYPE *EnumMasterClock)(IDirectMusic8*, DWORD, void*);
    HRESULT (STDMETHODCALLTYPE *GetMasterClock)(IDirectMusic8*, GUID*, IReferenceClock**);
    HRESULT (STDMETHODCALLTYPE *SetMasterClock)(IDirectMusic8*, REFGUID);
    HRESULT (STDMETHODCALLTYPE *Activate)(IDirectMusic8*, BOOL);
    HRESULT (STDMETHODCALLTYPE *GetDefaultPort)(IDirectMusic8*, GUID*);
    HRESULT (STDMETHODCALLTYPE *SetDirectSound)(IDirectMusic8*, void*, HWND);
    HRESULT (STDMETHODCALLTYPE *SetExternalMasterClock)(IDirectMusic8*, IReferenceClock*);
} IDirectMusic8Vtbl;
struct IDirectMusic8 { const IDirectMusic8Vtbl *lpVtbl; };

typedef struct {
    HRESULT (STDMETHODCALLTYPE *QueryInterface)(IDirectMusicPort*, REFIID, void**);
    ULONG   (STDMETHODCALLTYPE *AddRef)(IDirectMusicPort*);
    ULONG   (STDMETHODCALLTYPE *Release)(IDirectMusicPort*);
    HRESULT (STDMETHODCALLTYPE *PlayBuffer)(IDirectMusicPort*, IDirectMusicBuffer*);
    HRESULT (STDMETHODCALLTYPE *SetReadNotificationHandle)(IDirectMusicPort*, HANDLE);
    HRESULT (STDMETHODCALLTYPE *Read)(IDirectMusicPort*, IDirectMusicBuffer*);
    HRESULT (STDMETHODCALLTYPE *DownloadInstrument)(IDirectMusicPort*, IDirectMusicInstrument*, IDirectMusicDownloadedInstrument**, void*, DWORD);
    HRESULT (STDMETHODCALLTYPE *UnloadInstrument)(IDirectMusicPort*, IDirectMusicDownloadedInstrument*);
    HRESULT (STDMETHODCALLTYPE *GetLatencyClock)(IDirectMusicPort*, IReferenceClock**);
    HRESULT (STDMETHODCALLTYPE *GetRunningStats)(IDirectMusicPort*, void*);
    HRESULT (STDMETHODCALLTYPE *Compact)(IDirectMusicPort*);
    HRESULT (STDMETHODCALLTYPE *GetCaps)(IDirectMusicPort*, void*);
    HRESULT (STDMETHODCALLTYPE *DeviceIoControl)(IDirectMusicPort*, DWORD, void*, DWORD, void*, DWORD, DWORD*, OVERLAPPED*);
    HRESULT (STDMETHODCALLTYPE *SetNumChannelGroups)(IDirectMusicPort*, DWORD);
    HRESULT (STDMETHODCALLTYPE *GetNumChannelGroups)(IDirectMusicPort*, DWORD*);
    HRESULT (STDMETHODCALLTYPE *Activate)(IDirectMusicPort*, BOOL);
} IDirectMusicPortVtbl;
struct IDirectMusicPort { const IDirectMusicPortVtbl *lpVtbl; };

typedef struct {
    HRESULT (STDMETHODCALLTYPE *QueryInterface)(IDirectMusicBuffer*, REFIID, void**);
    ULONG   (STDMETHODCALLTYPE *AddRef)(IDirectMusicBuffer*);
    ULONG   (STDMETHODCALLTYPE *Release)(IDirectMusicBuffer*);
    HRESULT (STDMETHODCALLTYPE *Flush)(IDirectMusicBuffer*);
    HRESULT (STDMETHODCALLTYPE *TotalTime)(IDirectMusicBuffer*, REFERENCE_TIME*);
    HRESULT (STDMETHODCALLTYPE *PackStructured)(IDirectMusicBuffer*, REFERENCE_TIME, DWORD, DWORD);
    HRESULT (STDMETHODCALLTYPE *PackUnstructured)(IDirectMusicBuffer*, REFERENCE_TIME, DWORD, DWORD, BYTE*);
} IDirectMusicBufferVtbl;
struct IDirectMusicBuffer { const IDirectMusicBufferVtbl *lpVtbl; };

typedef struct {
    HRESULT (STDMETHODCALLTYPE *QueryInterface)(IDirectMusicLoader8*, REFIID, void**);
    ULONG   (STDMETHODCALLTYPE *AddRef)(IDirectMusicLoader8*);
    ULONG   (STDMETHODCALLTYPE *Release)(IDirectMusicLoader8*);
    HRESULT (STDMETHODCALLTYPE *GetObject)(IDirectMusicLoader8*, DMUS_OBJECTDESC*, REFIID, void**);
} IDirectMusicLoader8Vtbl;
struct IDirectMusicLoader8 { const IDirectMusicLoader8Vtbl *lpVtbl; };

typedef struct {
    HRESULT (STDMETHODCALLTYPE *QueryInterface)(IDirectMusicCollection*, REFIID, void**);
    ULONG   (STDMETHODCALLTYPE *AddRef)(IDirectMusicCollection*);
    ULONG   (STDMETHODCALLTYPE *Release)(IDirectMusicCollection*);
    HRESULT (STDMETHODCALLTYPE *GetInstrument)(IDirectMusicCollection*, DWORD, IDirectMusicInstrument**);
    HRESULT (STDMETHODCALLTYPE *EnumInstrument)(IDirectMusicCollection*, DWORD, DWORD*, WCHAR*, DWORD);
} IDirectMusicCollectionVtbl;
struct IDirectMusicCollection { const IDirectMusicCollectionVtbl *lpVtbl; };

typedef struct {
    HRESULT (STDMETHODCALLTYPE *QueryInterface)(IReferenceClock*, REFIID, void**);
    ULONG   (STDMETHODCALLTYPE *AddRef)(IReferenceClock*);
    ULONG   (STDMETHODCALLTYPE *Release)(IReferenceClock*);
    HRESULT (STDMETHODCALLTYPE *GetTime)(IReferenceClock*, REFERENCE_TIME*);
} IReferenceClockVtbl;
struct IReferenceClock { const IReferenceClockVtbl *lpVtbl; };

typedef struct {
    HRESULT (STDMETHODCALLTYPE *QueryInterface)(IKsControl*, REFIID, void**);
    ULONG   (STDMETHODCALLTYPE *AddRef)(IKsControl*);
    ULONG   (STDMETHODCALLTYPE *Release)(IKsControl*);
    HRESULT (STDMETHODCALLTYPE *KsProperty)(IKsControl*, KSIDENTIFIER*, ULONG, void*, ULONG, ULONG*);
} IKsControlVtbl;
struct IKsControl { const IKsControlVtbl *lpVtbl; };

static void check(const char *name, BOOL ok)
{
    printf("%s=%s\n", name, ok ? "ok" : "FAIL");
    fflush(stdout);
    if (!ok) ExitProcess(1);
}

int main(void)
{
    IDirectMusic8 *dm = NULL;
    IDirectMusicPort *port = NULL;
    IDirectMusicBuffer *buf = NULL;
    IDirectMusicLoader8 *loader = NULL;
    IDirectMusicCollection *coll = NULL;
    IDirectMusicInstrument *inst = NULL;
    IDirectMusicDownloadedInstrument *dl = NULL;
    IReferenceClock *clock = NULL;
    IKsControl *ks = NULL;
    GUID port_guid;
    DMUS_PORTPARAMS8 params;
    DMUS_BUFFERDESC bdesc;
    DMUS_OBJECTDESC odesc;
    KSIDENTIFIER prop;
    LONG vol = -300;
    ULONG ret_bytes = 0;
    REFERENCE_TIME rt = 0;
    BYTE gm_reset[] = { 0xf0, 0x7e, 0x7f, 0x09, 0x01, 0xf7 };

    check("coinit", SUCCEEDED(CoInitialize(NULL)));
    check("create-dm", SUCCEEDED(CoCreateInstance(&CLSID_DirectMusic, NULL, CLSCTX_INPROC_SERVER, &IID_IDirectMusic8, (void **)&dm)));
    check("set-dsound", SUCCEEDED(dm->lpVtbl->SetDirectSound(dm, NULL, NULL)));
    check("default-port", SUCCEEDED(dm->lpVtbl->GetDefaultPort(dm, &port_guid)));

    memset(&params, 0, sizeof(params));
    params.dwSize = sizeof(params);
    params.dwValidParams = 0x1f;
    params.dwVoices = 128;
    params.dwChannelGroups = 1;
    params.dwAudioChannels = 2;
    params.dwSampleRate = 44100;
    params.dwEffectFlags = 3;
    check("create-port", SUCCEEDED(dm->lpVtbl->CreatePort(dm, &port_guid, &params, &port, NULL)));

    check("qi-ks", SUCCEEDED(port->lpVtbl->QueryInterface(port, &IID_IKsControl, (void **)&ks)));
    memset(&prop, 0, sizeof(prop));
    prop.Set = GUID_DMUS_PROP_Volume;
    prop.Id = 0;
    prop.Flags = 2; /* KSPROPERTY_TYPE_SET */
    check("vol-set", SUCCEEDED(ks->lpVtbl->KsProperty(ks, &prop, sizeof(prop), &vol, sizeof(vol), &ret_bytes)));
    vol = 0;
    check("vol-restore", SUCCEEDED(ks->lpVtbl->KsProperty(ks, &prop, sizeof(prop), &vol, sizeof(vol), &ret_bytes)));

    check("activate", SUCCEEDED(port->lpVtbl->Activate(port, TRUE)));
    check("latency-clock", SUCCEEDED(port->lpVtbl->GetLatencyClock(port, &clock)));

    check("create-loader", SUCCEEDED(CoCreateInstance(&CLSID_DirectMusicLoader, NULL, CLSCTX_INPROC_SERVER, &IID_IDirectMusicLoader8, (void **)&loader)));
    memset(&odesc, 0, sizeof(odesc));
    odesc.dwSize = sizeof(odesc);
    odesc.dwValidData = 0x03; /* DMUS_OBJ_OBJECT | DMUS_OBJ_CLASS */
    odesc.guidObject = GUID_DefaultGMCollection;
    odesc.guidClass = CLSID_DirectMusicCollection;
    check("get-gm-collection", SUCCEEDED(loader->lpVtbl->GetObject(loader, &odesc, &IID_IDirectMusicCollection, (void **)&coll)));
    check("get-instrument", SUCCEEDED(coll->lpVtbl->GetInstrument(coll, 0, &inst)));
    check("download-instrument", SUCCEEDED(port->lpVtbl->DownloadInstrument(port, inst, &dl, NULL, 0)));

    memset(&bdesc, 0, sizeof(bdesc));
    bdesc.dwSize = sizeof(bdesc);
    bdesc.cbBuffer = 1024;
    check("create-buffer", SUCCEEDED(dm->lpVtbl->CreateMusicBuffer(dm, &bdesc, &buf, NULL)));
    check("clock-time", SUCCEEDED(clock->lpVtbl->GetTime(clock, &rt)));
    check("pack-sysex", SUCCEEDED(buf->lpVtbl->PackUnstructured(buf, rt, 1, sizeof(gm_reset), gm_reset)));
    check("pack-program", SUCCEEDED(buf->lpVtbl->PackStructured(buf, rt, 1, 0x000000c0)));
    check("pack-note", SUCCEEDED(buf->lpVtbl->PackStructured(buf, rt, 1, 0x00643c90)));
    check("play-buffer", SUCCEEDED(port->lpVtbl->PlayBuffer(port, buf)));

    Sleep(250);

    check("unload-instrument", SUCCEEDED(port->lpVtbl->UnloadInstrument(port, dl)));
    check("deactivate", SUCCEEDED(port->lpVtbl->Activate(port, FALSE)));

    ((IUnknown *)dl)->lpVtbl->Release((IUnknown *)dl);
    ((IUnknown *)inst)->lpVtbl->Release((IUnknown *)inst);
    coll->lpVtbl->Release(coll);
    loader->lpVtbl->Release(loader);
    buf->lpVtbl->Release(buf);
    clock->lpVtbl->Release(clock);
    ks->lpVtbl->Release(ks);
    port->lpVtbl->Release(port);
    dm->lpVtbl->Release(dm);
    CoUninitialize();
    puts("dmusic ok");
    return 0;
}
