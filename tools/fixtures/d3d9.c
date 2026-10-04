#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>

int _fltused;
void *memset(void *p, int byte, unsigned int n) {
    unsigned char *out = p;
    while (n--) *out++ = (unsigned char)byte;
    return p;
}
static DWORD results[64];
static UINT used;
static void record(DWORD value) { results[used++] = value; }
static void save(void) {
    DWORD wrote;
    HANDLE file = CreateFileA("d3d9-result.bin", GENERIC_WRITE, 0, NULL,
        CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(90);
    if (!WriteFile(file, results, used * sizeof(DWORD), &wrote, NULL) ||
        wrote != used * sizeof(DWORD)) ExitProcess(91);
    CloseHandle(file);
}
static void record_hr(HRESULT value) {
    record(value);
    if (FAILED(value)) { save(); ExitProcess(94); }
}
static DWORD device_refs(IDirect3DDevice9 *device) {
    DWORD n = IDirect3DDevice9_AddRef(device);
    IDirect3DDevice9_Release(device);
    return n - 1;
}
void mainCRTStartup(void) {
    IDirect3D9 *factory;
    IDirect3DDevice9 *device = NULL;
    IDirect3DSurface9 *back = NULL;
    IDirect3DTexture9 *texture = NULL;
    HWND window = CreateWindowExA(0, "STATIC", "D3D9 silent fixture", WS_OVERLAPPED,
        0, 0, 64, 64, NULL, NULL, GetModuleHandleA(NULL), NULL);
    D3DPRESENT_PARAMETERS parameters = {0};
    HRESULT hr;
    if (!window) ExitProcess(1);
    factory = Direct3DCreate9(D3D_SDK_VERSION);
    if (!factory) ExitProcess(2);
    parameters.BackBufferWidth = 32;
    parameters.BackBufferHeight = 32;
    parameters.BackBufferFormat = D3DFMT_A8R8G8B8;
    parameters.BackBufferCount = 1;
    parameters.SwapEffect = D3DSWAPEFFECT_DISCARD;
    parameters.hDeviceWindow = window;
    parameters.Windowed = TRUE;
    parameters.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    hr = IDirect3D9_CreateDevice(factory, 0, D3DDEVTYPE_HAL, window,
        D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE, &parameters, &device);
    record(hr);
    if (FAILED(hr)) { save(); ExitProcess(3); }
    record(device_refs(device));
    hr = IDirect3DDevice9_GetBackBuffer(device, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back);
    record(hr); record(device_refs(device));
    record(IDirect3DSurface9_AddRef(back));
    record(IDirect3DSurface9_Release(back));
    record(IDirect3DSurface9_Release(back));
    record(device_refs(device));
    hr = IDirect3DDevice9_CreateTexture(device, 2, 2, 1, 0,
        D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &texture, NULL);
    record(hr); record(device_refs(device));
    record(IDirect3DTexture9_AddRef(texture));
    record(IDirect3DTexture9_Release(texture));
    record(IDirect3DDevice9_SetTexture(device, 0, (IDirect3DBaseTexture9 *)texture));
    record(IDirect3DTexture9_AddRef(texture));
    record(IDirect3DTexture9_Release(texture));
    record(IDirect3DTexture9_Release(texture));
    record(device_refs(device));
    record(IDirect3DDevice9_SetTexture(device, 0, NULL));
    record(device_refs(device));
    parameters.BackBufferWidth = 16;
    parameters.BackBufferHeight = 12;
    record(IDirect3DDevice9_Reset(device, &parameters));
    record(parameters.BackBufferWidth);
    record(parameters.BackBufferHeight);
    record(parameters.BackBufferCount);
    record(parameters.BackBufferFormat);
    record(IDirect3DDevice9_GetBackBuffer(device, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back));
    {
        D3DSURFACE_DESC desc;
        record(IDirect3DSurface9_GetDesc(back, &desc));
        record(desc.Width); record(desc.Height); record(desc.Format);
        parameters.BackBufferWidth = 20;
        parameters.BackBufferHeight = 20;
        record(IDirect3DDevice9_Reset(device, &parameters));
        record(IDirect3DDevice9_TestCooperativeLevel(device));
    }
    record(IDirect3DSurface9_Release(back));
    parameters.BackBufferWidth = 16;
    parameters.BackBufferHeight = 12;
    record_hr(IDirect3DDevice9_Reset(device, &parameters));
    {
        struct Vertex {float x,y,z,rhw; DWORD color; float u,v;};
        const struct Vertex quad[4] = {
            {-0.5f,-0.5f,0,1,0xffffffffu,0,0},
            {15.5f,-0.5f,0,1,0xffffffffu,1,0},
            {-0.5f,11.5f,0,1,0xffffffffu,0,1},
            {15.5f,11.5f,0,1,0xffffffffu,1,1}
        };
        const DWORD texels[4]={0xff0000ffu,0xff00ff00u,0xffff0000u,0xffffffffu};
        IDirect3DSurface9 *readback=NULL;
        D3DLOCKED_RECT locked;
        DWORD pixels[16*12],wrote;
        HANDLE file;
        UINT x,y;
        record_hr(IDirect3DDevice9_CreateTexture(device,2,2,1,0,D3DFMT_A8R8G8B8,D3DPOOL_MANAGED,&texture,NULL));
        record_hr(IDirect3DTexture9_LockRect(texture,0,&locked,NULL,0));
        for(y=0;y<2;++y)for(x=0;x<2;++x) *((DWORD *)((BYTE *)locked.pBits+y*locked.Pitch)+x)=texels[y*2+x];
        record_hr(IDirect3DTexture9_UnlockRect(texture,0));
        record_hr(IDirect3DDevice9_SetTexture(device,0,(IDirect3DBaseTexture9 *)texture));
        record_hr(IDirect3DDevice9_SetFVF(device,D3DFVF_XYZRHW|D3DFVF_DIFFUSE|D3DFVF_TEX1));
        record_hr(IDirect3DDevice9_SetRenderState(device,D3DRS_LIGHTING,FALSE));
        record_hr(IDirect3DDevice9_SetRenderState(device,D3DRS_CULLMODE,D3DCULL_NONE));
        record_hr(IDirect3DDevice9_Clear(device,0,NULL,D3DCLEAR_TARGET,0xff010203u,1,0));
        record_hr(IDirect3DDevice9_BeginScene(device));
        record_hr(IDirect3DDevice9_DrawPrimitiveUP(device,D3DPT_TRIANGLESTRIP,2,quad,sizeof(quad[0])));
        record_hr(IDirect3DDevice9_EndScene(device));
        record_hr(IDirect3DDevice9_GetBackBuffer(device,0,0,D3DBACKBUFFER_TYPE_MONO,&back));
        record_hr(IDirect3DDevice9_CreateOffscreenPlainSurface(device,16,12,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&readback,NULL));
        record_hr(IDirect3DDevice9_GetRenderTargetData(device,back,readback));
        record_hr(IDirect3DSurface9_LockRect(readback,&locked,NULL,D3DLOCK_READONLY));
        for(y=0;y<12;++y)for(x=0;x<16;++x) pixels[y*16+x]=*((DWORD *)((BYTE *)locked.pBits+y*locked.Pitch)+x);
        file=CreateFileA("d3d9-pixels.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
        if(file==INVALID_HANDLE_VALUE||!WriteFile(file,pixels,sizeof(pixels),&wrote,NULL)||wrote!=sizeof(pixels))ExitProcess(92);
        CloseHandle(file);
        record_hr(IDirect3DSurface9_UnlockRect(readback));
        record_hr(IDirect3DSurface9_Release(readback));
        record_hr(IDirect3DSurface9_Release(back));
        record_hr(IDirect3DDevice9_Present(device,NULL,NULL,NULL,NULL));
        record_hr(IDirect3DTexture9_Release(texture));
    }
    record(IDirect3DDevice9_Release(device));
    record(IDirect3D9_Release(factory));
    {
        volatile DWORD guard = 0x12345678u;
        DWORD i;
        for(i=0;i<50000;++i) { guard ^= i; guard = guard * 1664525u + 1013904223u; }
        record(guard);
    }
    save();
    DestroyWindow(window);
    ExitProcess(0);
}
