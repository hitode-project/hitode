/* Authored RGBX conversion / render-target texture copy oracle.
 * Owned hidden window, no input. Output d3d9-texture-blit.bin is scalar. */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
int _fltused=0;
void *memset(void *p,int v,size_t n){volatile unsigned char *q=p;while(n--)*q++=(unsigned char)v;return p;}
static DWORD values[2048],used;
static void record(DWORD value){values[used++]=value;}
static void okay(HRESULT hr){record(hr);if(FAILED(hr))ExitProcess(100+used);}
static void capture(IDirect3DDevice9 *d,IDirect3DSurface9 *surface){IDirect3DSurface9 *read=0;D3DLOCKED_RECT lock;okay(IDirect3DDevice9_CreateOffscreenPlainSurface(d,16,16,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&read,0));okay(IDirect3DDevice9_GetRenderTargetData(d,surface,read));okay(IDirect3DSurface9_LockRect(read,&lock,0,D3DLOCK_READONLY));for(UINT y=0;y<16;y++)for(UINT x=0;x<16;x++)record(((DWORD*)((BYTE*)lock.pBits+y*lock.Pitch))[x]);okay(IDirect3DSurface9_UnlockRect(read));IDirect3DSurface9_Release(read);}
static void clear(IDirect3DDevice9 *d,IDirect3DSurface9 *surface,DWORD color){okay(IDirect3DDevice9_SetRenderTarget(d,0,surface));okay(IDirect3DDevice9_Clear(d,0,0,D3DCLEAR_TARGET,color,0,0));}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","authored texture copy",WS_POPUP,-32000,-32000,32,32,0,0,GetModuleHandleA(0),0);IDirect3D9 *factory=Direct3DCreate9(32);IDirect3DDevice9 *d=0;IDirect3DSurface9 *back=0,*surface=0,*x8=0,*a8=0;IDirect3DTexture9 *texture=0;D3DPRESENT_PARAMETERS p={0};DWORD state;RECT offset={4,4,12,12};
 if(!w||!factory)ExitProcess(1);p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_X8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=1;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;
 okay(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));okay(IDirect3DDevice9_GetBackBuffer(d,0,0,D3DBACKBUFFER_TYPE_MONO,&back));okay(IDirect3DDevice9_CreateTexture(d,16,16,1,D3DUSAGE_RENDERTARGET,D3DFMT_A8R8G8B8,D3DPOOL_DEFAULT,&texture,0));okay(IDirect3DTexture9_GetSurfaceLevel(texture,0,&surface));okay(IDirect3DDevice9_CreateRenderTarget(d,8,8,D3DFMT_X8R8G8B8,D3DMULTISAMPLE_NONE,0,FALSE,&x8,0));okay(IDirect3DDevice9_CreateRenderTarget(d,8,8,D3DFMT_A8R8G8B8,D3DMULTISAMPLE_NONE,0,FALSE,&a8,0));
 okay(IDirect3DDevice9_GetRenderState(d,D3DRS_DITHERENABLE,&state));record(state);okay(IDirect3DDevice9_SetRenderState(d,D3DRS_DITHERENABLE,FALSE));okay(IDirect3DDevice9_GetRenderState(d,D3DRS_COLORVERTEX,&state));record(state);okay(IDirect3DDevice9_SetRenderState(d,D3DRS_COLORVERTEX,FALSE));okay(IDirect3DDevice9_GetRenderState(d,D3DRS_COLORVERTEX,&state));record(state);okay(IDirect3DDevice9_SetRenderState(d,D3DRS_COLORVERTEX,TRUE));
 clear(d,surface,0x33112244);clear(d,back,0x00765a1f);RECT initial={0,0,16,12};okay(IDirect3DDevice9_StretchRect(d,back,0,surface,&initial,D3DTEXF_NONE));capture(d,surface);
 clear(d,surface,0x221122ff);clear(d,x8,0x00445522);okay(IDirect3DDevice9_StretchRect(d,x8,0,surface,&offset,D3DTEXF_POINT));capture(d,surface);
 clear(d,a8,0x44223355);okay(IDirect3DDevice9_StretchRect(d,a8,0,x8,0,D3DTEXF_NONE));clear(d,surface,0x11223344);okay(IDirect3DDevice9_StretchRect(d,x8,0,surface,&offset,D3DTEXF_LINEAR));capture(d,surface);
 okay(IDirect3DDevice9_SetRenderTarget(d,0,back));record(IDirect3DSurface9_Release(surface));record(IDirect3DTexture9_Release(texture));IDirect3DSurface9_Release(x8);IDirect3DSurface9_Release(a8);IDirect3DSurface9_Release(back);record(IDirect3DDevice9_Release(d));record(IDirect3D9_Release(factory));DestroyWindow(w);
 HANDLE file=CreateFileA("d3d9-texture-blit.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,used*4,&written,0)||written!=used*4)ExitProcess(2);CloseHandle(file);ExitProcess(0);
}
