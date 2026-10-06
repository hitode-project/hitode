/* Self-authored fixed-function border-addressing and mip-filter oracle. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int c,unsigned n){unsigned char *q=p;while(n--)*q++=(unsigned char)c;return p;}
static DWORD words[512],pixels[32],count,pixel_count;
static volatile DWORD execution_probe;
static void save_file(const char *name,const void *data,DWORD bytes){DWORD written;HANDLE f=CreateFileA(name,GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(f==INVALID_HANDLE_VALUE)ExitProcess(90);if(!WriteFile(f,data,bytes,&written,NULL)||written!=bytes)ExitProcess(91);CloseHandle(f);}
static void save(void){save_file("d3d9-border.bin",words,count*4);save_file("d3d9-border-pixels.bin",pixels,pixel_count*4);}
static void record(DWORD word){if(count==512)ExitProcess(92);words[count++]=word;}
static void ok(HRESULT hr){record(hr);if(FAILED(hr)){save();ExitProcess(94);}}
struct Vertex {float x,y,z,rhw;DWORD color;float u,v;};
static void sample(IDirect3DDevice9 *d,IDirect3DSurface9 *back,IDirect3DSurface9 *readback,float u,float v,float span){
 struct Vertex q[4];D3DLOCKED_RECT lock;UINT n;
 for(n=0;n<4;n++){q[n].x=(n&1)?0.5f:-0.5f;q[n].y=(n&2)?0.5f:-0.5f;q[n].z=0.5f;q[n].rhw=1;q[n].color=0xffffffffu;q[n].u=u+((n&1)?0.5f:-0.5f)*span;q[n].v=v+((n&2)?0.5f:-0.5f)*span;}
 ok(IDirect3DDevice9_Clear(d,0,NULL,D3DCLEAR_TARGET,0xff000000u,1,0));ok(IDirect3DDevice9_BeginScene(d));ok(IDirect3DDevice9_DrawPrimitiveUP(d,D3DPT_TRIANGLESTRIP,2,q,sizeof(q[0])));ok(IDirect3DDevice9_EndScene(d));
 ok(IDirect3DDevice9_GetRenderTargetData(d,back,readback));ok(IDirect3DSurface9_LockRect(readback,&lock,NULL,D3DLOCK_READONLY));pixels[pixel_count++]=*(DWORD*)lock.pBits;ok(IDirect3DSurface9_UnlockRect(readback));
}
static void sampler(IDirect3DDevice9 *d,D3DSAMPLERSTATETYPE kind,DWORD value){ok(IDirect3DDevice9_SetSamplerState(d,0,kind,value));}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(0,"STATIC","Border oracle",WS_OVERLAPPED,0,0,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);
 IDirect3D9 *factory=Direct3DCreate9(D3D_SDK_VERSION);IDirect3DDevice9 *d=NULL;IDirect3DSurface9 *back=NULL,*readback=NULL;IDirect3DTexture9 *image=NULL,*chain=NULL,*opaque=NULL;D3DPRESENT_PARAMETERS p={0};D3DLOCKED_RECT lock;UINT level,x,y;
 for(x=0;x<4096;x++)execution_probe+=x;
 if(!w||!factory)ExitProcess(1);p.BackBufferWidth=1;p.BackBufferHeight=1;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=TRUE;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;
 ok(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));ok(IDirect3DDevice9_GetBackBuffer(d,0,0,D3DBACKBUFFER_TYPE_MONO,&back));ok(IDirect3DDevice9_CreateOffscreenPlainSurface(d,1,1,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&readback,NULL));
 ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE|D3DFVF_TEX1));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_LIGHTING,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_ZENABLE,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_CULLMODE,D3DCULL_NONE));
 ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLOROP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLORARG1,D3DTA_TEXTURE));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAOP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAARG1,D3DTA_TEXTURE));
 ok(IDirect3DDevice9_CreateTexture(d,2,2,1,0,D3DFMT_A8R8G8B8,D3DPOOL_MANAGED,&image,NULL));ok(IDirect3DTexture9_LockRect(image,0,&lock,NULL,0));((DWORD*)lock.pBits)[0]=0xffff0000u;((DWORD*)lock.pBits)[1]=0xff0000ffu;((DWORD*)((BYTE*)lock.pBits+lock.Pitch))[0]=0xff00ff00u;((DWORD*)((BYTE*)lock.pBits+lock.Pitch))[1]=0xffffffffu;ok(IDirect3DTexture9_UnlockRect(image,0));ok(IDirect3DDevice9_SetTexture(d,0,(IDirect3DBaseTexture9*)image));
 sampler(d,D3DSAMP_ADDRESSU,D3DTADDRESS_BORDER);sampler(d,D3DSAMP_ADDRESSV,D3DTADDRESS_BORDER);sampler(d,D3DSAMP_BORDERCOLOR,0x80102040u);sampler(d,D3DSAMP_MAGFILTER,D3DTEXF_POINT);sampler(d,D3DSAMP_MINFILTER,D3DTEXF_POINT);sampler(d,D3DSAMP_MIPFILTER,D3DTEXF_NONE);
 sample(d,back,readback,-.1f,.25f,0);sample(d,back,readback,1,.25f,0);sample(d,back,readback,.25f,.25f,0);
 sampler(d,D3DSAMP_MAGFILTER,D3DTEXF_LINEAR);sample(d,back,readback,0,.25f,0);sample(d,back,readback,0,0,0);
 sampler(d,D3DSAMP_ADDRESSV,D3DTADDRESS_WRAP);sample(d,back,readback,0,1.25f,0);sampler(d,D3DSAMP_ADDRESSV,D3DTADDRESS_MIRROR);sample(d,back,readback,0,1.25f,0);sampler(d,D3DSAMP_ADDRESSV,D3DTADDRESS_CLAMP);sample(d,back,readback,0,-1,0);
 ok(IDirect3DDevice9_CreateTexture(d,2,2,1,0,D3DFMT_X8R8G8B8,D3DPOOL_MANAGED,&opaque,NULL));ok(IDirect3DTexture9_LockRect(opaque,0,&lock,NULL,0));for(y=0;y<2;y++)for(x=0;x<2;x++)((DWORD*)((BYTE*)lock.pBits+y*lock.Pitch))[x]=0x00ff0000u;ok(IDirect3DTexture9_UnlockRect(opaque,0));ok(IDirect3DDevice9_SetTexture(d,0,(IDirect3DBaseTexture9*)opaque));sample(d,back,readback,-1,.25f,0);sample(d,back,readback,0,.25f,0);
 ok(IDirect3DDevice9_CreateTexture(d,4,4,3,0,D3DFMT_A8R8G8B8,D3DPOOL_MANAGED,&chain,NULL));for(level=0;level<3;level++){const DWORD colors[3]={0xffff0000u,0xff00ff00u,0xff0000ffu};ok(IDirect3DTexture9_LockRect(chain,level,&lock,NULL,0));for(y=0;y<(4u>>level);y++)for(x=0;x<(4u>>level);x++)((DWORD*)((BYTE*)lock.pBits+y*lock.Pitch))[x]=colors[level];ok(IDirect3DTexture9_UnlockRect(chain,level));}
 ok(IDirect3DDevice9_SetTexture(d,0,(IDirect3DBaseTexture9*)chain));sampler(d,D3DSAMP_BORDERCOLOR,0xff000000u);sample(d,back,readback,0,.5f,1);
 sampler(d,D3DSAMP_MINFILTER,D3DTEXF_LINEAR);sampler(d,D3DSAMP_MIPFILTER,D3DTEXF_LINEAR);sample(d,back,readback,0,.5f,.7071067811865475f);
 sampler(d,D3DSAMP_MIPFILTER,D3DTEXF_POINT);sample(d,back,readback,0,.5f,1);
 sampler(d,D3DSAMP_MIPFILTER,D3DTEXF_NONE);sampler(d,D3DSAMP_MINFILTER,D3DTEXF_POINT);record(IDirect3DTexture9_SetLOD(chain,1));sample(d,back,readback,0,.5f,0);
 ok(IDirect3DDevice9_SetTexture(d,0,NULL));record(IDirect3DTexture9_Release(chain));record(IDirect3DTexture9_Release(opaque));record(IDirect3DTexture9_Release(image));record(IDirect3DSurface9_Release(readback));record(IDirect3DSurface9_Release(back));record(IDirect3DDevice9_Release(d));record(IDirect3D9_Release(factory));DestroyWindow(w);record(0xb0ade014u);save();ExitProcess(0);
}
