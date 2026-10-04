#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include <stddef.h>

_Static_assert(sizeof(D3DADAPTER_IDENTIFIER9) == 1100, "x86 SDK identifier size");
_Static_assert(__alignof(D3DADAPTER_IDENTIFIER9) == 4, "x86 SDK identifier alignment");
_Static_assert(offsetof(D3DADAPTER_IDENTIFIER9, DriverVersion) == 1056, "version offset");
_Static_assert(offsetof(D3DADAPTER_IDENTIFIER9, DeviceIdentifier) == 1080, "GUID offset");
_Static_assert(offsetof(D3DADAPTER_IDENTIFIER9, WHQLLevel) == 1096, "WHQL offset");
_Static_assert(sizeof(D3DCAPS9) == 304, "caps size");
int _fltused;
void *memset(void *p, int v, unsigned int n) { unsigned char *b=p; while(n--) *b++=(unsigned char)v; return p; }
void *memcpy(void *p, const void *q, unsigned int n) { unsigned char *a=p; const unsigned char *b=q; while(n--) *a++=*b++; return p; }
static DWORD words[128];
static UINT used;
static volatile DWORD warm;
static void save(void);
static void record(DWORD word) { if(used == 128) ExitProcess(98); words[used++]=word; save(); }
static int equal(const void *a,const void *b,UINT n) { const BYTE *p=a,*q=b; while(n--) if(*p++ != *q++) return 0; return 1; }
static void save(void) {
    DWORD wrote;
    HANDLE f=CreateFileA("d3d9-capabilities.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
    if(f==INVALID_HANDLE_VALUE) ExitProcess(90);
    if(!WriteFile(f,words,used*4,&wrote,NULL)||wrote!=used*4) ExitProcess(91);
    CloseHandle(f);
}
static void good(HRESULT hr) { record(hr); if(FAILED(hr)) { save(); ExitProcess(92); } }
void mainCRTStartup(void) {
    // Give asynchronous region compilation an actual repeated guest path.
    // This is coverage warm-up, not a rendering performance measurement.
    for(UINT i=0;i<65536;i++) warm+=i;
    if(warm!=0x7fff8000) ExitProcess(99);
    IDirect3D9 *d=Direct3DCreate9(D3D_SDK_VERSION);
    IDirect3DDevice9 *device=NULL;
    D3DDISPLAYMODE mode;
    struct { DWORD before; D3DADAPTER_IDENTIFIER9 value; DWORD after; } id;
    D3DADAPTER_IDENTIFIER9 again;
    struct { DWORD before; D3DCAPS9 value; DWORD after; } cap;
    D3DCAPS9 device_cap;
    D3DPRESENT_PARAMETERS pp={0};
    HWND hwnd;
    DWORD quality=0xdeadbeef;
    if(!d) ExitProcess(1);
    record(IDirect3D9_GetAdapterCount(d)>=1);
    id.before=id.after=0x12345678;
    good(IDirect3D9_GetAdapterIdentifier(d,0,0,&id.value));
    record(id.before==0x12345678 && id.after==0x12345678);
    record(id.value.Driver[0]!=0 && id.value.Description[0]!=0);
    good(IDirect3D9_GetAdapterIdentifier(d,0,0,&again));
    record(equal(&id.value,&again,sizeof(again)));
    memset(&id.value,0xa5,sizeof(id.value));
    record(IDirect3D9_GetAdapterIdentifier(d,0xffffffff,0,&id.value));
    memset(&again,0xa5,sizeof(again));
    record(equal(&id.value,&again,sizeof(again)));
    good(IDirect3D9_GetAdapterDisplayMode(d,0,&mode));
    record(mode.Width>0 && mode.Height>0 && mode.Format==D3DFMT_X8R8G8B8);
    record(IDirect3D9_GetAdapterDisplayMode(d,0xffffffff,&mode));
    record(IDirect3D9_GetAdapterModeCount(d,0,D3DFMT_X8R8G8B8)>0);
    record(IDirect3D9_GetAdapterModeCount(d,0xffffffff,D3DFMT_X8R8G8B8));
    record(IDirect3D9_GetAdapterModeCount(d,0,D3DFMT_UNKNOWN));
    good(IDirect3D9_EnumAdapterModes(d,0,D3DFMT_X8R8G8B8,0,&mode));
    record(mode.Width>0 && mode.Height>0 && mode.Format==D3DFMT_X8R8G8B8);
    record(IDirect3D9_EnumAdapterModes(d,0,D3DFMT_X8R8G8B8,0xffffffff,&mode));
    good(IDirect3D9_CheckDeviceType(d,0,D3DDEVTYPE_HAL,D3DFMT_X8R8G8B8,D3DFMT_UNKNOWN,TRUE));
    good(IDirect3D9_CheckDeviceType(d,0,D3DDEVTYPE_HAL,D3DFMT_X8R8G8B8,D3DFMT_A8R8G8B8,TRUE));
    record(IDirect3D9_CheckDeviceType(d,0xffffffff,D3DDEVTYPE_HAL,D3DFMT_X8R8G8B8,D3DFMT_UNKNOWN,TRUE));
    good(IDirect3D9_CheckDeviceFormat(d,0,D3DDEVTYPE_HAL,D3DFMT_X8R8G8B8,0,D3DRTYPE_TEXTURE,D3DFMT_A8R8G8B8));
    good(IDirect3D9_CheckDeviceFormat(d,0,D3DDEVTYPE_HAL,D3DFMT_X8R8G8B8,D3DUSAGE_DYNAMIC,D3DRTYPE_TEXTURE,D3DFMT_X8R8G8B8));
    good(IDirect3D9_CheckDeviceFormat(d,0,D3DDEVTYPE_HAL,D3DFMT_X8R8G8B8,0,D3DRTYPE_SURFACE,D3DFMT_A8R8G8B8));
    record(IDirect3D9_CheckDeviceFormat(d,0,D3DDEVTYPE_HAL,D3DFMT_X8R8G8B8,0,D3DRTYPE_TEXTURE,D3DFMT_UNKNOWN));
    good(IDirect3D9_CheckDeviceMultiSampleType(d,0,D3DDEVTYPE_HAL,D3DFMT_A8R8G8B8,TRUE,D3DMULTISAMPLE_NONE,&quality));
    record(quality);
    good(IDirect3D9_CheckDeviceMultiSampleType(d,0,D3DDEVTYPE_HAL,D3DFMT_A8R8G8B8,TRUE,D3DMULTISAMPLE_NONE,NULL));
    record(IDirect3D9_CheckDeviceMultiSampleType(d,0,D3DDEVTYPE_HAL,D3DFMT_A8R8G8B8,TRUE,(D3DMULTISAMPLE_TYPE)17,&quality));
    good(IDirect3D9_CheckDeviceFormatConversion(d,0,D3DDEVTYPE_HAL,D3DFMT_A8R8G8B8,D3DFMT_X8R8G8B8));
    cap.before=cap.after=0x87654321;
    good(IDirect3D9_GetDeviceCaps(d,0,D3DDEVTYPE_HAL,&cap.value));
    record(cap.before==0x87654321 && cap.after==0x87654321);
    record(cap.value.DeviceType==D3DDEVTYPE_HAL && cap.value.AdapterOrdinal==0);
    record(cap.value.MaxTextureWidth>=2048 && cap.value.MaxTextureHeight>=2048);
    record((cap.value.PresentationIntervals&D3DPRESENT_INTERVAL_IMMEDIATE)!=0);
    record(IDirect3D9_GetDeviceCaps(d,0xffffffff,D3DDEVTYPE_HAL,&cap.value));
    hwnd=CreateWindowExA(0,"STATIC","D3D9 capability oracle",WS_OVERLAPPED,0,0,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);
    if(!hwnd) ExitProcess(2);
    pp.BackBufferWidth=16; pp.BackBufferHeight=12; pp.BackBufferFormat=D3DFMT_A8R8G8B8;
    pp.BackBufferCount=1; pp.SwapEffect=D3DSWAPEFFECT_DISCARD; pp.Windowed=TRUE;
    pp.hDeviceWindow=hwnd; pp.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;
    good(IDirect3D9_CreateDevice(d,0,D3DDEVTYPE_HAL,hwnd,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&pp,&device));
    good(IDirect3DDevice9_GetDeviceCaps(device,&device_cap));
    record(device_cap.DeviceType==D3DDEVTYPE_HAL && device_cap.AdapterOrdinal==0);
    good(IDirect3DDevice9_GetDisplayMode(device,0,&mode));
    record(mode.Width>0 && mode.Height>0 && mode.Format==D3DFMT_X8R8G8B8);
    record(IDirect3DDevice9_GetDisplayMode(device,1,&mode));
    record(IDirect3DDevice9_Release(device));
    record(IDirect3D9_Release(d));
    DestroyWindow(hwnd);
    save(); ExitProcess(0);
}
