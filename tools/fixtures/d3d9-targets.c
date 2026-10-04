#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int c,unsigned n){unsigned char *q=p;while(n--)*q++=(unsigned char)c;return p;}
static const GUID texture_iid={0x85c31227,0x3de5,0x4f00,{0x9b,0x3a,0xf1,0x1a,0xc3,0x8c,0x18,0xb5}};
static const GUID device_iid={0xd0223b96,0xbf7a,0x43fd,{0x92,0xbd,0xa4,0x3b,0x0d,0x82,0xb9,0xeb}};
static const GUID unknown_iid={0,0,0,{0xc0,0,0,0,0,0,0,0x46}};
static DWORD out[256],n,pixels[592],np;
#define REC(x) out[n++]=(DWORD)(x)
static DWORD refs(IUnknown *p){DWORD r=IUnknown_AddRef(p);IUnknown_Release(p);return r-1;}
static void save_file(const char *name,const void *data,DWORD size){DWORD wrote;HANDLE h=CreateFileA(name,GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);if(h==INVALID_HANDLE_VALUE||!WriteFile(h,data,size,&wrote,0)||wrote!=size)ExitProcess(90);CloseHandle(h);}
static void save(void){save_file("d3d9-targets.bin",out,n*4);save_file("d3d9-targets-pixels.bin",pixels,np*4);}
static void ok(HRESULT h){REC(h);if(FAILED(h)){save();ExitProcess(91);}}
static void capture(IDirect3DDevice9*d,IDirect3DSurface9*s,UINT width,UINT height){
 IDirect3DSurface9*r=0;D3DLOCKED_RECT l;UINT x,y;
 ok(IDirect3DDevice9_CreateOffscreenPlainSurface(d,width,height,21,2,&r,0));
 ok(IDirect3DDevice9_GetRenderTargetData(d,s,r));
 ok(IDirect3DSurface9_LockRect(r,&l,0,D3DLOCK_READONLY));
 for(y=0;y<height;y++)for(x=0;x<width;x++)pixels[np++]=((DWORD*)((BYTE*)l.pBits+y*l.Pitch))[x];
 ok(IDirect3DSurface9_UnlockRect(r));REC(IDirect3DSurface9_Release(r));
}
struct Vertex{float x,y,z,rhw;DWORD color;float u,v;};
static void quad(IDirect3DDevice9*d,float width,float height,DWORD color){
 struct Vertex v[4]={{-.5f,-.5f,0,1,color,0,0},{width-.5f,-.5f,0,1,color,1,0},{-.5f,height-.5f,0,1,color,0,1},{width-.5f,height-.5f,0,1,color,1,1}};
 ok(IDirect3DDevice9_BeginScene(d));ok(IDirect3DDevice9_DrawPrimitiveUP(d,D3DPT_TRIANGLESTRIP,2,v,sizeof(v[0])));ok(IDirect3DDevice9_EndScene(d));
}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","target oracle",WS_OVERLAPPED,-32000,-32000,32,32,0,0,GetModuleHandleA(0),0);
 IDirect3D9*f=Direct3DCreate9(32);IDirect3DDevice9*d=0,*parent=0;IDirect3DTexture9*t=0,*c=0;IDirect3DSurface9*s=0,*a=0,*back=0,*standalone=0;IUnknown*u=0;
 D3DPRESENT_PARAMETERS p={0};D3DVIEWPORT9 view={0};D3DSURFACE_DESC desc;
 p.BackBufferWidth=16;p.BackBufferHeight=16;p.BackBufferFormat=21;p.BackBufferCount=1;p.SwapEffect=1;p.hDeviceWindow=w;p.Windowed=1;p.PresentationInterval=0x80000000;
 if(!w||!f)ExitProcess(2);ok(IDirect3D9_CreateDevice(f,0,1,w,0x22,&p,&d));
 ok(IDirect3DDevice9_CreateTexture(d,8,8,1,1,21,0,&t,0));REC(refs((IUnknown*)d));REC(refs((IUnknown*)t));
 ok(IDirect3DTexture9_GetSurfaceLevel(t,0,&s));REC(refs((IUnknown*)d));REC(refs((IUnknown*)t));REC(refs((IUnknown*)s));
 ok(IDirect3DSurface9_QueryInterface(s,&unknown_iid,(void**)&u));REC(u==(IUnknown*)s);REC(u==(IUnknown*)t);REC(IUnknown_Release(u));
 REC(IDirect3DSurface9_AddRef(s));REC(refs((IUnknown*)t));REC(IDirect3DSurface9_Release(s));
 ok(IDirect3DSurface9_GetContainer(s,&texture_iid,(void**)&c));REC(c==t);REC(refs((IUnknown*)t));REC(IDirect3DTexture9_Release(c));
 ok(IDirect3DTexture9_GetSurfaceLevel(t,0,&a));REC(a==s);REC(IDirect3DSurface9_Release(a));
 ok(IDirect3DSurface9_GetDevice(s,&parent));REC(parent==d);REC(IDirect3DDevice9_Release(parent));
 REC(IDirect3DSurface9_GetType(s));REC(IDirect3DTexture9_GetType(t));
 ok(IDirect3DSurface9_GetDesc(s,&desc));REC(desc.Width);REC(desc.Height);REC(desc.Usage);
 ok(IDirect3DDevice9_GetBackBuffer(d,0,0,0,&back));
 ok(IDirect3DDevice9_Clear(d,0,0,D3DCLEAR_TARGET,0xff112233u,1,0));
 ok(IDirect3DDevice9_SetRenderTarget(d,0,s));REC(refs((IUnknown*)s));REC(refs((IUnknown*)t));
 ok(IDirect3DDevice9_GetViewport(d,&view));REC(view.Width);REC(view.Height);
 view.X=1;view.Y=1;view.Width=2;view.Height=2;ok(IDirect3DDevice9_SetViewport(d,&view));
 ok(IDirect3DDevice9_SetRenderTarget(d,0,s));ok(IDirect3DDevice9_GetViewport(d,&view));REC(view.X);REC(view.Y);REC(view.Width);REC(view.Height);
 ok(IDirect3DDevice9_Clear(d,0,0,D3DCLEAR_TARGET,0xff112233u,1,0));
 ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE|D3DFVF_TEX1));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_LIGHTING,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_CULLMODE,D3DCULL_NONE));
 quad(d,8,8,0xffaa2200u);capture(d,s,8,8);capture(d,back,16,16);
 ok(IDirect3DDevice9_SetRenderTarget(d,0,back));ok(IDirect3DDevice9_SetTexture(d,0,(IDirect3DBaseTexture9*)t));quad(d,16,16,0xffffffffu);capture(d,back,16,16);ok(IDirect3DDevice9_SetTexture(d,0,0));
 // Retain only a device binding, then revive the level and its container.
 ok(IDirect3DDevice9_SetRenderTarget(d,0,s));REC(IDirect3DTexture9_Release(t));REC(IDirect3DSurface9_Release(s));REC(refs((IUnknown*)d));
 ok(IDirect3DDevice9_GetRenderTarget(d,0,&a));REC(a==s);REC(refs((IUnknown*)a));ok(IDirect3DSurface9_GetContainer(a,&texture_iid,(void**)&c));REC(refs((IUnknown*)c));REC(IDirect3DTexture9_Release(c));
 ok(IDirect3DDevice9_SetRenderTarget(d,0,back));REC(IDirect3DSurface9_Release(a));
 ok(IDirect3DDevice9_CreateRenderTarget(d,4,4,21,0,0,FALSE,&standalone,0));
 ok(IDirect3DSurface9_GetContainer(standalone,&device_iid,(void**)&parent));REC(parent==d);REC(IDirect3DDevice9_Release(parent));
 ok(IDirect3DDevice9_SetRenderTarget(d,0,standalone));ok(IDirect3DDevice9_Clear(d,0,0,D3DCLEAR_TARGET,0xff00cc44u,1,0));capture(d,standalone,4,4);
 REC(IDirect3DSurface9_Release(standalone));REC(IDirect3DSurface9_Release(back));
 ok(IDirect3DDevice9_Reset(d,&p));ok(IDirect3DDevice9_GetRenderTarget(d,0,&a));ok(IDirect3DSurface9_GetDesc(a,&desc));REC(desc.Width);REC(desc.Height);REC(IDirect3DSurface9_Release(a));
 // Texture and surface locks operate on the same backing and lock state.
 {
  D3DLOCKED_RECT locked;
  ok(IDirect3DDevice9_CreateTexture(d,2,2,1,0,21,D3DPOOL_MANAGED,&t,0));
  ok(IDirect3DTexture9_GetSurfaceLevel(t,0,&s));
  ok(IDirect3DSurface9_LockRect(s,&locked,0,0));*((DWORD*)locked.pBits)=0xffab5432u;
  ok(IDirect3DTexture9_UnlockRect(t,0));
  ok(IDirect3DTexture9_LockRect(t,0,&locked,0,D3DLOCK_READONLY));REC(*((DWORD*)locked.pBits));
  ok(IDirect3DSurface9_UnlockRect(s));
  REC(IDirect3DDevice9_SetRenderTarget(d,0,s));
  REC(IDirect3DSurface9_GetContainer(s,&device_iid,(void**)&parent));REC(parent==0);
  REC(IDirect3DTexture9_GetSurfaceLevel(t,1,&a));REC(a==0);
  REC(IDirect3DSurface9_Release(s));REC(IDirect3DTexture9_Release(t));
 }
 ok(IDirect3DDevice9_Present(d,0,0,0,0));REC(IDirect3DDevice9_Release(d));REC(IDirect3D9_Release(f));
 {volatile DWORD guard=0x12345678u;DWORD i;for(i=0;i<50000;i++){guard^=i;guard=guard*1664525u+1013904223u;}REC(guard);}
 save();DestroyWindow(w);ExitProcess(0);
}
