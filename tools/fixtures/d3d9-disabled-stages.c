/* Repository-owned disabled texture-stage state oracle. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p, int c, unsigned n) { unsigned char *q=p; while(n--) *q++=(unsigned char)c; return p; }
static DWORD words[1024];
static UINT count;
static void save(void) {
    DWORD wrote;
    HANDLE f=CreateFileA("d3d9-disabled-stages.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
    if(f==INVALID_HANDLE_VALUE) ExitProcess(90);
    if(!WriteFile(f,words,count*4,&wrote,NULL)||wrote!=count*4) ExitProcess(91);
    CloseHandle(f);
}
static void record(DWORD x) { if(count==1024) ExitProcess(92); words[count++]=x; }
static void good(HRESULT hr) { record(hr); if(FAILED(hr)) { save(); ExitProcess(93); } }
static void stage(IDirect3DDevice9 *d, DWORD n, D3DTEXTURESTAGESTATETYPE kind) {
    DWORD value;
    good(IDirect3DDevice9_GetTextureStageState(d,n,kind,&value)); record(value);
}
void mainCRTStartup(void) {
    HWND w=CreateWindowExA(WS_EX_NOACTIVATE,"STATIC","D3D9 disabled stage oracle",WS_OVERLAPPED,-32000,-32000,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);
    IDirect3D9 *factory=Direct3DCreate9(D3D_SDK_VERSION);
    IDirect3DDevice9 *d=NULL;
    IDirect3DStateBlock9 *block=NULL;
    D3DPRESENT_PARAMETERS p={0};
    DWORD n,kind,value;
    p.BackBufferWidth=16; p.BackBufferHeight=12; p.BackBufferFormat=D3DFMT_A8R8G8B8;
    p.BackBufferCount=1; p.SwapEffect=D3DSWAPEFFECT_DISCARD; p.hDeviceWindow=w; p.Windowed=TRUE;
    if(!w||!factory) ExitProcess(1);
    good(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));
    for(n=1;n<8;n++) for(kind=1;kind<=6;kind++) {
        stage(d,n,(D3DTEXTURESTAGESTATETYPE)kind);
        good(IDirect3DDevice9_SetTextureStageState(d,n,(D3DTEXTURESTAGESTATETYPE)kind,(kind==1||kind==4)?D3DTOP_DISABLE:D3DTA_DIFFUSE));
        stage(d,n,(D3DTEXTURESTAGESTATETYPE)kind);
    }
    good(IDirect3DDevice9_GetRenderState(d,D3DRS_WRAP0,&value)); record(value);
    good(IDirect3DDevice9_SetRenderState(d,D3DRS_WRAP0,0));
    good(IDirect3DDevice9_GetRenderState(d,D3DRS_WRAP0,&value)); record(value);
    for(n=1;n<8;n++) for(kind=2;kind<=6;kind++) if(kind!=4) {
        good(IDirect3DDevice9_SetTextureStageState(d,n,(D3DTEXTURESTAGESTATETYPE)kind,D3DTA_CONSTANT|D3DTA_COMPLEMENT|D3DTA_ALPHAREPLICATE));
        stage(d,n,(D3DTEXTURESTAGESTATETYPE)kind);
    }
    for(kind=D3DSBT_ALL;kind<=D3DSBT_VERTEXSTATE;kind++) {
        good(IDirect3DDevice9_SetTextureStageState(d,2,D3DTSS_COLORARG1,D3DTA_TEXTURE));
        good(IDirect3DDevice9_CreateStateBlock(d,(D3DSTATEBLOCKTYPE)kind,&block));
        good(IDirect3DDevice9_SetTextureStageState(d,2,D3DTSS_COLORARG1,D3DTA_DIFFUSE));
        good(IDirect3DStateBlock9_Apply(block)); stage(d,2,D3DTSS_COLORARG1);
        record(IDirect3DStateBlock9_Release(block)); block=NULL;
    }
    good(IDirect3DDevice9_SetTextureStageState(d,1,D3DTSS_COLORARG2,D3DTA_CURRENT));
    good(IDirect3DDevice9_BeginStateBlock(d));
    good(IDirect3DDevice9_SetTextureStageState(d,1,D3DTSS_COLORARG2,D3DTA_DIFFUSE));
    good(IDirect3DDevice9_SetTextureStageState(d,7,D3DTSS_COLOROP,D3DTOP_DISABLE));
    stage(d,1,D3DTSS_COLORARG2);
    good(IDirect3DDevice9_EndStateBlock(d,&block));
    good(IDirect3DDevice9_SetTextureStageState(d,2,D3DTSS_COLORARG2,D3DTA_TEXTURE));
    good(IDirect3DStateBlock9_Apply(block)); stage(d,1,D3DTSS_COLORARG2); stage(d,2,D3DTSS_COLORARG2);
    good(IDirect3DDevice9_SetTextureStageState(d,1,D3DTSS_COLORARG2,D3DTA_TEXTURE));
    good(IDirect3DStateBlock9_Capture(block));
    good(IDirect3DDevice9_SetTextureStageState(d,1,D3DTSS_COLORARG2,D3DTA_DIFFUSE));
    good(IDirect3DStateBlock9_Apply(block)); stage(d,1,D3DTSS_COLORARG2);
    record(IDirect3DStateBlock9_Release(block)); record(IDirect3DDevice9_Release(d)); record(IDirect3D9_Release(factory));
    DestroyWindow(w); record(0x57a90001); save(); ExitProcess(0);
}
