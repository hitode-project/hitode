/* Self-authored shader oracle: 16x12 readbacks and exact COM/constant records. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include "d3d9-shaders-generated.h"

int _fltused;
void *memset(void *p, int c, unsigned n) { unsigned char *q=p; while(n--) *q++=(unsigned char)c; return p; }
static DWORD words[512]; static UINT count; static HANDLE pixel_file;
static void record(DWORD v) { if(count>=512) ExitProcess(98); words[count++]=v; }
static void save(void) { DWORD wrote; HANDLE f=CreateFileA("d3d9-shaders.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL); if(f==INVALID_HANDLE_VALUE) ExitProcess(90); if(!WriteFile(f,words,count*4,&wrote,NULL)||wrote!=count*4) ExitProcess(91); CloseHandle(f); }
static void ok(HRESULT hr) { record(hr); if(FAILED(hr)){ save(); ExitProcess(94); } }
static void capture(IDirect3DDevice9 *d,IDirect3DSurface9 *back) {
 IDirect3DSurface9 *s=NULL; D3DLOCKED_RECT lock; UINT y; DWORD wrote;
 ok(IDirect3DDevice9_CreateOffscreenPlainSurface(d,16,12,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&s,NULL));
 ok(IDirect3DDevice9_GetRenderTargetData(d,back,s)); ok(IDirect3DSurface9_LockRect(s,&lock,NULL,D3DLOCK_READONLY));
 for(y=0;y<12;++y) if(!WriteFile(pixel_file,(BYTE *)lock.pBits+y*lock.Pitch,64,&wrote,NULL)||wrote!=64) ExitProcess(92);
 ok(IDirect3DSurface9_UnlockRect(s)); record(IDirect3DSurface9_Release(s));
}
struct Vertex { float x,y,z; DWORD color; float u,v; };
struct ScreenVertex { float x,y,z,rhw; DWORD color; float u,v; };
static const struct Vertex quad[4]={{-1,1,.5f,0xff806040u,0,0},{1,1,.5f,0xff806040u,1,0},{-1,-1,.5f,0xff806040u,0,1},{1,-1,.5f,0xff806040u,1,1}};
static const struct ScreenVertex screen_quad[4]={{-.5f,-.5f,.5f,1,0xff806040u,0,0},{15.5f,-.5f,.5f,1,0xff806040u,1,0},{-.5f,11.5f,.5f,1,0xff806040u,0,1},{15.5f,11.5f,.5f,1,0xff806040u,1,1}};
static void draw(IDirect3DDevice9 *d,BOOL screen) { ok(IDirect3DDevice9_Clear(d,0,NULL,D3DCLEAR_TARGET,0xff000000u,1,0)); ok(IDirect3DDevice9_BeginScene(d)); ok(IDirect3DDevice9_DrawPrimitiveUP(d,D3DPT_TRIANGLESTRIP,2,screen?(const void *)screen_quad:(const void *)quad,screen?sizeof(struct ScreenVertex):sizeof(struct Vertex))); ok(IDirect3DDevice9_EndScene(d)); }
static void constant_records(IDirect3DDevice9 *d) {
 const float floats[8]={.5f,.25f,1,1,0,0,0,0}; const int integers[4]={-1,2,-3,4}; const BOOL booleans[2]={TRUE,FALSE}; DWORD values[8]; UINT i;
 ok(IDirect3DDevice9_SetPixelShaderConstantF(d,0,floats,2)); ok(IDirect3DDevice9_GetPixelShaderConstantF(d,0,(float *)values,2)); for(i=0;i<8;++i)record(values[i]);
 ok(IDirect3DDevice9_SetVertexShaderConstantI(d,2,integers,1)); ok(IDirect3DDevice9_GetVertexShaderConstantI(d,2,(int *)values,1)); for(i=0;i<4;++i)record(values[i]);
 ok(IDirect3DDevice9_SetPixelShaderConstantB(d,3,booleans,2)); ok(IDirect3DDevice9_GetPixelShaderConstantB(d,3,(BOOL *)values,2)); for(i=0;i<2;++i)record(values[i]);
}
void mainCRTStartup(void) {
 HWND w=CreateWindowExA(0,"STATIC","D3D9 shader oracle",WS_OVERLAPPED,0,0,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);
 IDirect3D9 *f=Direct3DCreate9(D3D_SDK_VERSION); IDirect3DDevice9 *d=NULL; IDirect3DSurface9 *back=NULL; IDirect3DTexture9 *texture=NULL;
 IDirect3DVertexShader9 *v20=NULL,*v30=NULL,*queried_vs=NULL; IDirect3DPixelShader9 *p20=NULL,*pt=NULL,*p30=NULL,*queried_ps=NULL;
 D3DPRESENT_PARAMETERS p={0}; D3DLOCKED_RECT lock; UINT bytes=0,i; DWORD copied[512]; const DWORD texels[4]={0xff204080u,0xff80c020u,0xffe04080u,0xff40a0e0u};
 if(!w||!f)ExitProcess(1);pixel_file=CreateFileA("d3d9-shaders-pixels.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(pixel_file==INVALID_HANDLE_VALUE)ExitProcess(2);
 p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=TRUE;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;
 ok(IDirect3D9_CreateDevice(f,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));ok(IDirect3DDevice9_GetBackBuffer(d,0,0,D3DBACKBUFFER_TYPE_MONO,&back));
 ok(IDirect3DDevice9_SetRenderState(d,D3DRS_LIGHTING,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_ZENABLE,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_CULLMODE,D3DCULL_NONE));
 ok(IDirect3DDevice9_CreateVertexShader(d,(const DWORD *)vs20,&v20));ok(IDirect3DDevice9_CreatePixelShader(d,(const DWORD *)ps20_color,&p20));ok(IDirect3DDevice9_CreatePixelShader(d,(const DWORD *)ps20_texture,&pt));
 ok(IDirect3DVertexShader9_GetFunction(v20,NULL,&bytes));record(bytes);if(bytes>sizeof(copied))ExitProcess(3);ok(IDirect3DVertexShader9_GetFunction(v20,copied,&bytes));for(i=0;i<bytes/4;++i)if(copied[i]!=((const DWORD *)vs20)[i])ExitProcess(4);
 bytes=0;ok(IDirect3DPixelShader9_GetFunction(p20,NULL,&bytes));record(bytes);if(bytes>sizeof(copied))ExitProcess(5);ok(IDirect3DPixelShader9_GetFunction(p20,copied,&bytes));for(i=0;i<bytes/4;++i)if(copied[i]!=((const DWORD *)ps20_color)[i])ExitProcess(6);
 ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZ|D3DFVF_DIFFUSE|D3DFVF_TEX1));ok(IDirect3DDevice9_SetVertexShader(d,v20));ok(IDirect3DDevice9_SetPixelShader(d,p20));
 ok(IDirect3DDevice9_GetVertexShader(d,&queried_vs));record(queried_vs==v20);record(IDirect3DVertexShader9_Release(queried_vs));ok(IDirect3DDevice9_GetPixelShader(d,&queried_ps));record(queried_ps==p20);record(IDirect3DPixelShader9_Release(queried_ps));constant_records(d);
 draw(d,FALSE);capture(d,back);
 ok(IDirect3DDevice9_CreateTexture(d,2,2,1,0,D3DFMT_A8R8G8B8,D3DPOOL_MANAGED,&texture,NULL));ok(IDirect3DTexture9_LockRect(texture,0,&lock,NULL,0));for(i=0;i<4;++i)((DWORD *)((BYTE *)lock.pBits+(i/2)*lock.Pitch))[i%2]=texels[i];ok(IDirect3DTexture9_UnlockRect(texture,0));
 ok(IDirect3DDevice9_SetTexture(d,0,(IDirect3DBaseTexture9 *)texture));ok(IDirect3DDevice9_SetSamplerState(d,0,D3DSAMP_MINFILTER,D3DTEXF_POINT));ok(IDirect3DDevice9_SetSamplerState(d,0,D3DSAMP_MAGFILTER,D3DTEXF_POINT));ok(IDirect3DDevice9_SetSamplerState(d,0,D3DSAMP_ADDRESSU,D3DTADDRESS_CLAMP));ok(IDirect3DDevice9_SetSamplerState(d,0,D3DSAMP_ADDRESSV,D3DTADDRESS_CLAMP));
 ok(IDirect3DDevice9_SetPixelShader(d,pt));draw(d,FALSE);capture(d,back);
 ok(IDirect3DDevice9_SetTexture(d,0,NULL));ok(IDirect3DDevice9_SetPixelShader(d,NULL));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLOROP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLORARG1,D3DTA_DIFFUSE));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAOP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAARG1,D3DTA_DIFFUSE));draw(d,FALSE);capture(d,back);
 ok(IDirect3DDevice9_SetVertexShader(d,NULL));ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE|D3DFVF_TEX1));ok(IDirect3DDevice9_SetPixelShader(d,p20));draw(d,TRUE);capture(d,back);
 ok(IDirect3DDevice9_CreateVertexShader(d,(const DWORD *)vs30,&v30));ok(IDirect3DDevice9_CreatePixelShader(d,(const DWORD *)ps30_flow,&p30));ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZ|D3DFVF_DIFFUSE|D3DFVF_TEX1));ok(IDirect3DDevice9_SetVertexShader(d,v30));ok(IDirect3DDevice9_SetPixelShader(d,p30));draw(d,FALSE);capture(d,back);
 ok(IDirect3DDevice9_SetVertexShader(d,NULL));ok(IDirect3DDevice9_SetPixelShader(d,NULL));record(IDirect3DVertexShader9_Release(v20));record(IDirect3DVertexShader9_Release(v30));record(IDirect3DPixelShader9_Release(p20));record(IDirect3DPixelShader9_Release(pt));record(IDirect3DPixelShader9_Release(p30));record(IDirect3DTexture9_Release(texture));record(IDirect3DSurface9_Release(back));record(IDirect3DDevice9_Release(d));record(IDirect3D9_Release(f));CloseHandle(pixel_file);DestroyWindow(w);record(0x5ade0005u);save();ExitProcess(0);
}
