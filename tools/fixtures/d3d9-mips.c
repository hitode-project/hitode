#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int c,unsigned n){unsigned char *q=p;while(n--)*q++=(unsigned char)c;return p;}
static DWORD out[1024],n,pixels[1024],np;
#define REC(x) out[n++]=(DWORD)(x)
static const IID texture_iid={0x85c31227,0x3de5,0x4f00,{0x9b,0x3a,0xf1,0x1a,0xc3,0x8c,0x18,0xb5}};
static void save_file(const char *name,const void *data,DWORD size){DWORD wrote;HANDLE h=CreateFileA(name,GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);if(h==INVALID_HANDLE_VALUE||!WriteFile(h,data,size,&wrote,0)||wrote!=size)ExitProcess(90);CloseHandle(h);}
static void save(void){save_file("d3d9-mips.bin",out,n*4);save_file("d3d9-mips-pixels.bin",pixels,np*4);}
static void ok(HRESULT h){REC(h);if(FAILED(h)){save();ExitProcess(91);}}
static DWORD refs(IUnknown*p){DWORD r=IUnknown_AddRef(p);IUnknown_Release(p);return r-1;}
static void desc(IDirect3DTexture9*t,UINT level){D3DSURFACE_DESC d;ok(IDirect3DTexture9_GetLevelDesc(t,level,&d));REC(d.Width);REC(d.Height);REC(d.Format);REC(d.Type);REC(d.Usage);REC(d.Pool);}
static void fill(IDirect3DTexture9*t,UINT level,DWORD color){D3DLOCKED_RECT l;D3DSURFACE_DESC d;UINT x,y;ok(IDirect3DTexture9_GetLevelDesc(t,level,&d));ok(IDirect3DTexture9_LockRect(t,level,&l,0,0));REC(l.Pitch>=d.Width*4);for(y=0;y<d.Height;y++)for(x=0;x<d.Width;x++)((DWORD*)((BYTE*)l.pBits+y*l.Pitch))[x]=color;ok(IDirect3DTexture9_UnlockRect(t,level));}
static void capture(IDirect3DDevice9*d,IDirect3DSurface9*s,UINT w,UINT h){IDirect3DSurface9*r=0;D3DLOCKED_RECT l;UINT x,y;ok(IDirect3DDevice9_CreateOffscreenPlainSurface(d,w,h,21,2,&r,0));ok(IDirect3DDevice9_GetRenderTargetData(d,s,r));ok(IDirect3DSurface9_LockRect(r,&l,0,D3DLOCK_READONLY));for(y=0;y<h;y++)for(x=0;x<w;x++)pixels[np++]=((DWORD*)((BYTE*)l.pBits+y*l.Pitch))[x];ok(IDirect3DSurface9_UnlockRect(r));REC(IDirect3DSurface9_Release(r));}
struct Vertex {float x,y,z,rhw;DWORD color;float u,v;};
static void draw(IDirect3DDevice9*d,UINT w,UINT h,float uv){struct Vertex v[4]={{-.5f,-.5f,0,1,0xffffffff,0,0},{w-.5f,-.5f,0,1,0xffffffff,uv,0},{-.5f,h-.5f,0,1,0xffffffff,0,uv},{w-.5f,h-.5f,0,1,0xffffffff,uv,uv}};ok(IDirect3DDevice9_BeginScene(d));ok(IDirect3DDevice9_DrawPrimitiveUP(d,D3DPT_TRIANGLESTRIP,2,v,sizeof(v[0])));ok(IDirect3DDevice9_EndScene(d));}
static void sample(IDirect3DDevice9*d,IDirect3DTexture9*t,UINT w,UINT h,DWORD filter,DWORD first,float uv){IDirect3DSurface9*r=0;ok(IDirect3DDevice9_CreateRenderTarget(d,w,h,21,0,0,FALSE,&r,0));ok(IDirect3DDevice9_SetRenderTarget(d,0,r));ok(IDirect3DDevice9_SetTexture(d,0,(IDirect3DBaseTexture9*)t));ok(IDirect3DDevice9_SetSamplerState(d,0,D3DSAMP_MIPFILTER,filter));ok(IDirect3DDevice9_SetSamplerState(d,0,D3DSAMP_MAXMIPLEVEL,first));ok(IDirect3DDevice9_Clear(d,0,0,1,0xff000000,1,0));draw(d,w,h,uv);capture(d,r,w,h);REC(IDirect3DSurface9_Release(r));}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","mip oracle",WS_OVERLAPPED,-32000,-32000,32,32,0,0,GetModuleHandleA(0),0);
 IDirect3D9*f=Direct3DCreate9(32);IDirect3DDevice9*d=0;IDirect3DTexture9*t=0,*limited=0,*rt=0,*parent=0;IDirect3DSurface9*s=0,*same=0,*back=0;D3DPRESENT_PARAMETERS p={0};D3DLOCKED_RECT l,a;D3DSURFACE_DESC sd;DWORD i,value;RECT sub={1,0,2,1};
 if(!w||!f)ExitProcess(2);p.BackBufferWidth=8;p.BackBufferHeight=4;p.BackBufferFormat=21;p.BackBufferCount=1;p.SwapEffect=1;p.hDeviceWindow=w;p.Windowed=1;p.PresentationInterval=0x80000000;ok(IDirect3D9_CreateDevice(f,0,1,w,0x22,&p,&d));
 REC(0x4d495031);ok(IDirect3DDevice9_GetBackBuffer(d,0,0,0,&back));
 {D3DCAPS9 c;ok(IDirect3DDevice9_GetDeviceCaps(d,&c));REC((c.TextureCaps&D3DPTEXTURECAPS_MIPMAP)!=0);REC((c.TextureFilterCaps&D3DPTFILTERCAPS_MIPFPOINT)!=0);REC((c.TextureFilterCaps&D3DPTFILTERCAPS_MIPFLINEAR)!=0);}
 ok(IDirect3DDevice9_CreateTexture(d,8,4,0,0,21,1,&t,0));REC(IDirect3DTexture9_GetLevelCount(t));
 for(i=0;i<4;i++){
  desc(t,i);ok(IDirect3DTexture9_GetSurfaceLevel(t,i,&s));ok(IDirect3DTexture9_GetSurfaceLevel(t,i,&same));REC(s==same);REC(refs((IUnknown*)s)==refs((IUnknown*)t));REC(IDirect3DSurface9_Release(same));
  ok(IDirect3DSurface9_GetContainer(s,&texture_iid,(void**)&parent));REC(parent==t);REC(IDirect3DTexture9_Release(parent));
  ok(IDirect3DSurface9_GetDesc(s,&sd));REC(sd.Width);REC(sd.Height);REC(IDirect3DSurface9_Release(s));
 }
 s=(IDirect3DSurface9*)0xdeadbeef;REC(IDirect3DTexture9_GetSurfaceLevel(t,4,&s));REC(s==0);REC(IDirect3DTexture9_GetLevelDesc(t,4,&sd));
 fill(t,0,0xffff0000);fill(t,1,0xff00ff00);fill(t,2,0xff0000ff);fill(t,3,0xffffffff);
 // Texture and surface locks share the actual selected level's state/address.
 ok(IDirect3DTexture9_GetSurfaceLevel(t,1,&s));ok(IDirect3DSurface9_LockRect(s,&l,&sub,0));REC(IDirect3DTexture9_LockRect(t,1,&a,0,0));*(DWORD*)l.pBits=0xff00ffff;ok(IDirect3DTexture9_LockRect(t,2,&a,0,D3DLOCK_READONLY));REC(*(DWORD*)a.pBits);ok(IDirect3DTexture9_UnlockRect(t,2));ok(IDirect3DSurface9_UnlockRect(s));ok(IDirect3DTexture9_LockRect(t,1,&a,&sub,D3DLOCK_READONLY));REC(a.pBits==l.pBits);REC(*(DWORD*)a.pBits);ok(IDirect3DTexture9_UnlockRect(t,1));REC(IDirect3DSurface9_Release(s));REC(IDirect3DTexture9_UnlockRect(t,1));
 // Restore uniform colors for deterministic mip selection.
 fill(t,1,0xff00ff00);
 ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE|D3DFVF_TEX1));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_LIGHTING,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_CULLMODE,D3DCULL_NONE));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLOROP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLORARG1,D3DTA_TEXTURE));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAOP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAARG1,D3DTA_TEXTURE));ok(IDirect3DDevice9_SetSamplerState(d,0,D3DSAMP_ADDRESSU,D3DTADDRESS_CLAMP));ok(IDirect3DDevice9_SetSamplerState(d,0,D3DSAMP_ADDRESSV,D3DTADDRESS_CLAMP));
 ok(IDirect3DDevice9_GetSamplerState(d,0,D3DSAMP_MAXMIPLEVEL,&value));REC(value);REC(IDirect3DTexture9_GetLOD(t));
 sample(d,t,1,1,D3DTEXF_NONE,0,1);sample(d,t,1,1,D3DTEXF_POINT,0,1);sample(d,t,8,4,D3DTEXF_NONE,2,1);sample(d,t,1,1,D3DTEXF_NONE,100,1);sample(d,t,8,4,D3DTEXF_POINT,2,1);sample(d,t,8,4,D3DTEXF_LINEAR,2,1);sample(d,t,8,4,D3DTEXF_POINT,3,1);sample(d,t,2,1,D3DTEXF_LINEAR,0,1.41421356237f);
 REC(IDirect3DTexture9_SetLOD(t,2));REC(IDirect3DTexture9_GetLOD(t));sample(d,t,8,4,D3DTEXF_NONE,0,1);REC(IDirect3DTexture9_SetLOD(t,100));REC(IDirect3DTexture9_GetLOD(t));sample(d,t,8,4,D3DTEXF_NONE,0,1);REC(IDirect3DTexture9_SetLOD(t,0));REC(IDirect3DTexture9_GetLOD(t));
 // A partial chain and a nonmanaged render-target chain share all their levels.
 ok(IDirect3DDevice9_CreateTexture(d,8,4,2,0,21,1,&limited,0));REC(IDirect3DTexture9_GetLevelCount(limited));desc(limited,1);REC(IDirect3DTexture9_SetLOD(limited,100));REC(IDirect3DTexture9_GetLOD(limited));REC(IDirect3DTexture9_Release(limited));
 limited=0;ok(IDirect3DDevice9_CreateTexture(d,3,5,0,0,21,1,&limited,0));REC(IDirect3DTexture9_GetLevelCount(limited));for(i=0;i<3;i++)desc(limited,i);REC(IDirect3DTexture9_Release(limited));
 limited=0;REC(IDirect3DDevice9_CreateTexture(d,8,4,5,0,21,1,&limited,0));REC(limited==0);if(limited){REC(IDirect3DTexture9_GetLevelCount(limited));REC(IDirect3DTexture9_Release(limited));}
 ok(IDirect3DDevice9_SetTexture(d,0,0));ok(IDirect3DDevice9_SetRenderTarget(d,0,back));ok(IDirect3DDevice9_CreateTexture(d,8,4,0,1,21,0,&rt,0));REC(IDirect3DTexture9_GetLevelCount(rt));REC(IDirect3DTexture9_SetLOD(rt,2));REC(IDirect3DTexture9_GetLOD(rt));
 for(i=0;i<4;i++){DWORD colors[4]={0xffff0000,0xff00ff00,0xff0000ff,0xffffffff};ok(IDirect3DTexture9_GetSurfaceLevel(rt,i,&s));ok(IDirect3DDevice9_SetRenderTarget(d,0,s));ok(IDirect3DDevice9_Clear(d,0,0,1,colors[i],1,0));REC(IDirect3DSurface9_Release(s));}
 // GetRenderTargetData writes through a SYSTEMMEM texture level view. Its
 // heap storage and lock state remain owned only by the controlling texture.
 limited=0;ok(IDirect3DDevice9_CreateTexture(d,8,4,0,0,21,2,&limited,0));REC(IDirect3DTexture9_GetLevelCount(limited));desc(limited,1);ok(IDirect3DTexture9_GetSurfaceLevel(limited,1,&same));ok(IDirect3DTexture9_GetSurfaceLevel(rt,1,&s));ok(IDirect3DDevice9_GetRenderTargetData(d,s,same));ok(IDirect3DSurface9_LockRect(same,&l,0,D3DLOCK_READONLY));for(i=0;i<8;i++)pixels[np++]=((DWORD*)((BYTE*)l.pBits+(i/4)*l.Pitch))[i%4];ok(IDirect3DTexture9_UnlockRect(limited,1));REC(IDirect3DSurface9_Release(same));REC(IDirect3DTexture9_Release(limited));REC(IDirect3DSurface9_Release(s));
 for(i=0;i<4;i++){ok(IDirect3DTexture9_GetSurfaceLevel(rt,i,&s));ok(IDirect3DSurface9_GetDesc(s,&sd));capture(d,s,sd.Width,sd.Height);REC(IDirect3DSurface9_Release(s));}
 sample(d,rt,1,1,D3DTEXF_POINT,0,1);
 ok(IDirect3DDevice9_SetTexture(d,0,0));ok(IDirect3DDevice9_SetRenderTarget(d,0,back));REC(IDirect3DTexture9_Release(rt));REC(IDirect3DTexture9_Release(t));REC(IDirect3DSurface9_Release(back));REC(IDirect3DDevice9_Release(d));REC(IDirect3D9_Release(f));REC(DestroyWindow(w));REC(0xcafef00d);save();ExitProcess(0);
}
