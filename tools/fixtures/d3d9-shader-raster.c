/* Self-authored shader oracle: 16x12 readbacks and exact COM/constant records. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include "d3d9-shader-raster-generated.h"

int _fltused;
void *memset(void *p, int c, unsigned n) { unsigned char *q=p; while(n--) *q++=(unsigned char)c; return p; }
static DWORD words[512]; static UINT count; static HANDLE pixel_file;
static void record(DWORD v) { if(count>=512) ExitProcess(98); words[count++]=v; }
static void save(void) { DWORD wrote; HANDLE f=CreateFileA("d3d9-shader-raster.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL); if(f==INVALID_HANDLE_VALUE) ExitProcess(90); if(!WriteFile(f,words,count*4,&wrote,NULL)||wrote!=count*4) ExitProcess(91); CloseHandle(f); }
static void ok(HRESULT hr) { record(hr); if(FAILED(hr)){ save(); ExitProcess(94); } }
static void capture(IDirect3DDevice9 *d,IDirect3DSurface9 *back) {
 IDirect3DSurface9 *s=NULL; D3DLOCKED_RECT lock; UINT y; DWORD wrote;
 ok(IDirect3DDevice9_CreateOffscreenPlainSurface(d,16,12,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&s,NULL));
 ok(IDirect3DDevice9_GetRenderTargetData(d,back,s)); ok(IDirect3DSurface9_LockRect(s,&lock,NULL,D3DLOCK_READONLY));
 for(y=0;y<12;++y) if(!WriteFile(pixel_file,(BYTE *)lock.pBits+y*lock.Pitch,64,&wrote,NULL)||wrote!=64) ExitProcess(92);
 ok(IDirect3DSurface9_UnlockRect(s)); record(IDirect3DSurface9_Release(s));
}

struct Vertex {float x,y,z;DWORD color;};
static const struct Vertex quad[4]={{-1,1,.5f,0xffff0000u},{1,1,.5f,0xffff0000u},{-1,-1,.5f,0xffff0000u},{1,-1,.5f,0xffff0000u}};
static const struct Vertex point={0,0,.5f,0xffff0000u};
void mainCRTStartup(void){
 volatile UINT warmup=0;while(warmup<4096)warmup++;
 HWND w=CreateWindowExA(0,"STATIC","Shader raster oracle",WS_OVERLAPPED,0,0,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);
 IDirect3D9 *f=Direct3DCreate9(D3D_SDK_VERSION);IDirect3DDevice9 *d=NULL;IDirect3DSurface9 *back=NULL;IDirect3DVertexShader9 *vs=NULL,*clip=NULL;
 D3DPRESENT_PARAMETERS p={0};const float settings[4]={.25f,4,0,0};const float plane[4]={1,0,0,0};UINT n;
 if(!w||!f)ExitProcess(1);
 pixel_file=CreateFileA("d3d9-shader-raster-pixels.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(pixel_file==INVALID_HANDLE_VALUE)ExitProcess(2);
 p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=TRUE;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;
 ok(IDirect3D9_CreateDevice(f,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));ok(IDirect3DDevice9_GetBackBuffer(d,0,0,D3DBACKBUFFER_TYPE_MONO,&back));
 ok(IDirect3DDevice9_CreateVertexShader(d,(const DWORD*)raster_vs,&vs));ok(IDirect3DDevice9_CreateVertexShader(d,(const DWORD*)clip_vs,&clip));ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZ|D3DFVF_DIFFUSE));ok(IDirect3DDevice9_SetVertexShader(d,vs));ok(IDirect3DDevice9_SetVertexShaderConstantF(d,0,settings,1));
 ok(IDirect3DDevice9_SetRenderState(d,D3DRS_LIGHTING,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_ZENABLE,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_CULLMODE,D3DCULL_NONE));
 ok(IDirect3DDevice9_SetRenderState(d,D3DRS_FOGCOLOR,0xff0000ffu));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_FOGTABLEMODE,D3DFOG_NONE));ok(IDirect3DDevice9_SetClipPlane(d,0,plane));
 for(n=0;n<3;n++){
  ok(IDirect3DDevice9_SetVertexShader(d,n==1?clip:vs));
  ok(IDirect3DDevice9_SetRenderState(d,D3DRS_FOGENABLE,n==0));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_CLIPPLANEENABLE,n==1?1:0));
  ok(IDirect3DDevice9_Clear(d,0,NULL,D3DCLEAR_TARGET,0xff000000u,1,0));ok(IDirect3DDevice9_BeginScene(d));
  ok(IDirect3DDevice9_DrawPrimitiveUP(d,n==2?D3DPT_POINTLIST:D3DPT_TRIANGLESTRIP,n==2?1:2,n==2?(const void*)&point:(const void*)quad,sizeof(struct Vertex)));
  ok(IDirect3DDevice9_EndScene(d));capture(d,back);
 }
 ok(IDirect3DDevice9_SetVertexShader(d,NULL));record(IDirect3DVertexShader9_Release(vs));record(IDirect3DVertexShader9_Release(clip));record(IDirect3DSurface9_Release(back));record(IDirect3DDevice9_Release(d));record(IDirect3D9_Release(f));CloseHandle(pixel_file);DestroyWindow(w);record(0x5ade0003u);save();ExitProcess(0);
}
