#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int c,unsigned n){unsigned char *q=p;while(n--)*q++=(unsigned char)c;return p;}
static DWORD out[512],n,pixels[2112],np;
#define REC(x) out[n++]=(DWORD)(x)
static void save_file(const char *name,const void *data,DWORD size){DWORD wrote;HANDLE h=CreateFileA(name,GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);if(h==INVALID_HANDLE_VALUE||!WriteFile(h,data,size,&wrote,0)||wrote!=size)ExitProcess(90);CloseHandle(h);}
static void save(void){save_file("d3d9-depth.bin",out,n*4);save_file("d3d9-depth-pixels.bin",pixels,np*4);}
static void ok(HRESULT h){REC(h);if(FAILED(h)){save();ExitProcess(91);}}
static DWORD refs(IUnknown*p){DWORD r=IUnknown_AddRef(p);IUnknown_Release(p);return r-1;}
static void capture(IDirect3DDevice9*d,IDirect3DSurface9*s){
 IDirect3DSurface9*r=0;D3DLOCKED_RECT l;UINT x,y;
 ok(IDirect3DDevice9_CreateOffscreenPlainSurface(d,16,12,21,2,&r,0));
 ok(IDirect3DDevice9_GetRenderTargetData(d,s,r));
 ok(IDirect3DSurface9_LockRect(r,&l,0,D3DLOCK_READONLY));
 for(y=0;y<12;y++)for(x=0;x<16;x++)pixels[np++]=((DWORD*)((BYTE*)l.pBits+y*l.Pitch))[x];
 ok(IDirect3DSurface9_UnlockRect(r));REC(IDirect3DSurface9_Release(r));
}
struct Vertex {float x,y,z,rhw;DWORD color;};
static void quad(IDirect3DDevice9*d,float z,DWORD color){
 struct Vertex v[4]={{-.5f,-.5f,z,1,color},{15.5f,-.5f,z,1,color},{-.5f,11.5f,z,1,color},{15.5f,11.5f,z,1,color}};
 ok(IDirect3DDevice9_BeginScene(d));ok(IDirect3DDevice9_DrawPrimitiveUP(d,D3DPT_TRIANGLESTRIP,2,v,sizeof(v[0])));ok(IDirect3DDevice9_EndScene(d));
}
static void opposite_faces(IDirect3DDevice9*d){
 // Screen-space clockwise left triangle and counterclockwise right triangle.
 struct Vertex v[6]={{-.5f,-.5f,.5f,1,0xffffffff},{7.5f,-.5f,.5f,1,0xffffffff},{-.5f,11.5f,.5f,1,0xffffffff},
  {7.5f,-.5f,.5f,1,0xffffffff},{7.5f,11.5f,.5f,1,0xffffffff},{15.5f,-.5f,.5f,1,0xffffffff}};
 ok(IDirect3DDevice9_BeginScene(d));ok(IDirect3DDevice9_DrawPrimitiveUP(d,D3DPT_TRIANGLELIST,2,v,sizeof(v[0])));ok(IDirect3DDevice9_EndScene(d));
}
static void state(IDirect3DDevice9*d,D3DRENDERSTATETYPE s,DWORD v){ok(IDirect3DDevice9_SetRenderState(d,s,v));}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","depth oracle",WS_OVERLAPPED,-32000,-32000,32,32,0,0,GetModuleHandleA(0),0);
 IDirect3D9*f=Direct3DCreate9(32);IDirect3DDevice9*d=0,*parent=0;IDirect3DSurface9*depth=0,*same=0,*explicitDepth=0,*back=0,*color=0;
 D3DPRESENT_PARAMETERS p={0};D3DSURFACE_DESC desc;DWORD value=0;UINT i;
 p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=21;p.BackBufferCount=1;p.SwapEffect=1;p.hDeviceWindow=w;p.Windowed=1;p.PresentationInterval=0x80000000;p.EnableAutoDepthStencil=TRUE;p.AutoDepthStencilFormat=D3DFMT_D24S8;
 if(!w||!f)ExitProcess(2);ok(IDirect3D9_CreateDevice(f,0,1,w,0x22,&p,&d));
 ok(IDirect3DDevice9_GetDepthStencilSurface(d,&depth));REC(refs((IUnknown*)depth));REC(refs((IUnknown*)d));
 ok(IDirect3DSurface9_GetDesc(depth,&desc));REC(desc.Format);REC(desc.Type);REC(desc.Usage);REC(desc.Pool);REC(desc.Width);REC(desc.Height);
 ok(IDirect3DSurface9_GetDevice(depth,&parent));REC(parent==d);REC(IDirect3DDevice9_Release(parent));
 ok(IDirect3DDevice9_GetRenderState(d,D3DRS_ZENABLE,&value));REC(value);
 ok(IDirect3DDevice9_GetRenderState(d,D3DRS_STENCILENABLE,&value));REC(value);
 ok(IDirect3DDevice9_GetBackBuffer(d,0,0,0,&back));
 ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE));state(d,D3DRS_LIGHTING,FALSE);state(d,D3DRS_CULLMODE,D3DCULL_NONE);
 ok(IDirect3DDevice9_Clear(d,0,0,7,0xff000000,1,0));
 quad(d,.5f,0xffff0000);quad(d,.75f,0xff00ff00);capture(d,back);
 quad(d,.25f,0xff0000ff);capture(d,back);
 // Same independent depth is kept while changing the color target.
 ok(IDirect3DDevice9_CreateRenderTarget(d,16,12,21,0,0,FALSE,&color,0));
 ok(IDirect3DDevice9_SetRenderTarget(d,0,color));ok(IDirect3DDevice9_Clear(d,0,0,1,0xff000000,1,0));
 quad(d,.5f,0xffff0000);capture(d,color);quad(d,.125f,0xffffffff);capture(d,color);
 ok(IDirect3DDevice9_SetRenderTarget(d,0,back));REC(IDirect3DSurface9_Release(color));
 // Stencil replacement then a matching/nonmatching test. Store just contract
 // results here; the device test covers all operations and masks independently.
 state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILENABLE,TRUE);
 ok(IDirect3DDevice9_Clear(d,0,0,7,0xff000000,1,0));
 state(d,D3DRS_COLORWRITEENABLE,0);state(d,D3DRS_STENCILFUNC,D3DCMP_ALWAYS);state(d,D3DRS_STENCILPASS,D3DSTENCILOP_REPLACE);state(d,D3DRS_STENCILREF,7);quad(d,.5f,0xffffffff);
 state(d,D3DRS_COLORWRITEENABLE,15);state(d,D3DRS_STENCILPASS,D3DSTENCILOP_KEEP);state(d,D3DRS_STENCILFUNC,D3DCMP_EQUAL);quad(d,.5f,0xff00ff00);
 state(d,D3DRS_STENCILFUNC,D3DCMP_NOTEQUAL);quad(d,.5f,0xffff0000);capture(d,back);
 state(d,D3DRS_STENCILENABLE,FALSE);
 // Depth writes disabled must leave the original clear depth intact.
 state(d,D3DRS_ZENABLE,TRUE);state(d,D3DRS_ZWRITEENABLE,FALSE);
 ok(IDirect3DDevice9_Clear(d,0,0,7,0xff000000,1,0));
 quad(d,.25f,0xffff0000);quad(d,.75f,0xff00ff00);capture(d,back);
 state(d,D3DRS_ZWRITEENABLE,TRUE);quad(d,.5f,0xff0000ff);quad(d,.75f,0xffff0000);capture(d,back);
 // Write mask preserves the clear's high nibble; read mask ignores ref high bits.
 state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILENABLE,TRUE);
 ok(IDirect3DDevice9_Clear(d,0,0,7,0xff000000,1,0xa0));
 state(d,D3DRS_COLORWRITEENABLE,0);state(d,D3DRS_STENCILFUNC,D3DCMP_ALWAYS);state(d,D3DRS_STENCILPASS,D3DSTENCILOP_REPLACE);
 state(d,D3DRS_STENCILWRITEMASK,0x0f);state(d,D3DRS_STENCILMASK,0xff);state(d,D3DRS_STENCILREF,7);quad(d,.5f,0xffffffff);
 state(d,D3DRS_COLORWRITEENABLE,15);state(d,D3DRS_STENCILPASS,D3DSTENCILOP_KEEP);state(d,D3DRS_STENCILFUNC,D3DCMP_EQUAL);state(d,D3DRS_STENCILREF,0xa7);quad(d,.5f,0xff00ffff);capture(d,back);
 state(d,D3DRS_STENCILMASK,0x0f);state(d,D3DRS_STENCILREF,0xb7);quad(d,.5f,0xffff00ff);capture(d,back);
 // Failed Z test updates stencil via ZFAIL and leaves color untouched.
 state(d,D3DRS_STENCILMASK,0xff);state(d,D3DRS_STENCILWRITEMASK,0xff);state(d,D3DRS_ZENABLE,TRUE);state(d,D3DRS_ZFUNC,D3DCMP_LESS);
 state(d,D3DRS_STENCILFUNC,D3DCMP_ALWAYS);state(d,D3DRS_STENCILZFAIL,D3DSTENCILOP_REPLACE);state(d,D3DRS_STENCILREF,3);
 ok(IDirect3DDevice9_Clear(d,0,0,7,0xff000000,.1f,0));quad(d,.5f,0xffff0000);
 state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILZFAIL,D3DSTENCILOP_KEEP);state(d,D3DRS_STENCILFUNC,D3DCMP_EQUAL);quad(d,.5f,0xffffff00);capture(d,back);
 // The two face operation sets must select different spatial results.
 ok(IDirect3DDevice9_Clear(d,0,0,7,0xff000000,1,1));
 state(d,D3DRS_COLORWRITEENABLE,0);state(d,D3DRS_TWOSIDEDSTENCILMODE,TRUE);
 state(d,D3DRS_STENCILFUNC,D3DCMP_ALWAYS);state(d,D3DRS_STENCILPASS,D3DSTENCILOP_REPLACE);state(d,D3DRS_STENCILREF,3);
 state(d,D3DRS_CCW_STENCILFUNC,D3DCMP_ALWAYS);state(d,D3DRS_CCW_STENCILFAIL,D3DSTENCILOP_KEEP);state(d,D3DRS_CCW_STENCILZFAIL,D3DSTENCILOP_KEEP);state(d,D3DRS_CCW_STENCILPASS,D3DSTENCILOP_ZERO);
 opposite_faces(d);
 state(d,D3DRS_TWOSIDEDSTENCILMODE,FALSE);state(d,D3DRS_COLORWRITEENABLE,15);state(d,D3DRS_STENCILPASS,D3DSTENCILOP_KEEP);state(d,D3DRS_STENCILFUNC,D3DCMP_EQUAL);
 quad(d,.5f,0xffff0000);state(d,D3DRS_STENCILREF,0);quad(d,.5f,0xff00ff00);capture(d,back);
 state(d,D3DRS_STENCILENABLE,FALSE);
 for(i=0;i<2;i++){
  D3DFORMAT format=i?D3DFMT_D24X8:D3DFMT_D16;
  ok(IDirect3DDevice9_CreateDepthStencilSurface(d,16,12,format,0,0,FALSE,&explicitDepth,0));
  ok(IDirect3DSurface9_GetDesc(explicitDepth,&desc));REC(desc.Format);REC(desc.Usage);REC(desc.Pool);
  ok(IDirect3DDevice9_SetDepthStencilSurface(d,explicitDepth));REC(refs((IUnknown*)explicitDepth));
  ok(IDirect3DDevice9_GetDepthStencilSurface(d,&same));REC(same==explicitDepth);REC(IDirect3DSurface9_Release(same));
  ok(IDirect3DDevice9_Clear(d,0,0,2,0,1,0));REC(IDirect3DDevice9_Clear(d,0,0,4,0,1,0));
  REC(IDirect3DSurface9_Release(explicitDepth));
  ok(IDirect3DDevice9_GetDepthStencilSurface(d,&same));REC(same==explicitDepth);REC(refs((IUnknown*)same));REC(refs((IUnknown*)d));REC(IDirect3DSurface9_Release(same));
 }
 ok(IDirect3DDevice9_SetDepthStencilSurface(d,0));same=(IDirect3DSurface9*)0xdeadbeef;REC(IDirect3DDevice9_GetDepthStencilSurface(d,&same));REC(same==0);
 ok(IDirect3DDevice9_SetDepthStencilSurface(d,depth));REC(IDirect3DSurface9_Release(depth));
 ok(IDirect3DDevice9_GetDepthStencilSurface(d,&same));REC(same==depth);REC(refs((IUnknown*)same));REC(IDirect3DSurface9_Release(same));
 {D3DPRESENT_PARAMETERS attempt=p;REC(IDirect3DDevice9_Reset(d,&attempt));}
 REC(IDirect3DSurface9_Release(back));ok(IDirect3DDevice9_Reset(d,&p));
 ok(IDirect3DDevice9_GetRenderState(d,D3DRS_ZENABLE,&value));REC(value);
 ok(IDirect3DDevice9_GetDepthStencilSurface(d,&same));REC(IDirect3DSurface9_Release(same));
 REC(IDirect3DDevice9_Release(d));REC(IDirect3D9_Release(f));REC(0xcafef00d);save();DestroyWindow(w);ExitProcess(0);
}
