// Reuse authored fixture recording/drawing helpers, with an independent entry.
#define mainCRTStartup unused_targets_entry
#include "d3d9-targets.c"
#undef mainCRTStartup
void mainCRTStartup(void){
 HWND w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","opaque alpha oracle",WS_OVERLAPPED,-32000,-32000,32,32,0,0,GetModuleHandleA(0),0);
 IDirect3D9*f=Direct3DCreate9(32);IDirect3DDevice9*d=0;IDirect3DTexture9*t=0;IDirect3DSurface9*s=0,*back=0;
 D3DPRESENT_PARAMETERS p={0};
 p.BackBufferWidth=16;p.BackBufferHeight=16;p.BackBufferFormat=21;p.BackBufferCount=1;p.SwapEffect=1;p.hDeviceWindow=w;p.Windowed=1;p.PresentationInterval=0x80000000;
 if(!w||!f)ExitProcess(2);ok(IDirect3D9_CreateDevice(f,0,1,w,0x22,&p,&d));
 ok(IDirect3DDevice9_GetBackBuffer(d,0,0,0,&back));
 ok(IDirect3DDevice9_CreateTexture(d,2,2,1,D3DUSAGE_RENDERTARGET,D3DFMT_X8R8G8B8,D3DPOOL_DEFAULT,&t,0));
 ok(IDirect3DTexture9_GetSurfaceLevel(t,0,&s));ok(IDirect3DDevice9_SetRenderTarget(d,0,s));
 ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE|D3DFVF_TEX1));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_LIGHTING,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_CULLMODE,D3DCULL_NONE));
 ok(IDirect3DDevice9_Clear(d,0,0,D3DCLEAR_TARGET,0x00112233u,1,0));quad(d,2,2,0x80ffffffu);
 ok(IDirect3DDevice9_SetRenderState(d,D3DRS_ALPHABLENDENABLE,TRUE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_SRCBLEND,D3DBLEND_DESTALPHA));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_DESTBLEND,D3DBLEND_ZERO));quad(d,2,2,0x40aa2200u);
 ok(IDirect3DDevice9_SetRenderTarget(d,0,back));ok(IDirect3DDevice9_Clear(d,0,0,D3DCLEAR_TARGET,0xff112233u,1,0));
 ok(IDirect3DDevice9_SetTexture(d,0,(IDirect3DBaseTexture9*)t));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_SRCBLEND,D3DBLEND_SRCALPHA));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_DESTBLEND,D3DBLEND_INVSRCALPHA));quad(d,16,16,0xffffffffu);
 capture(d,back,16,16);ok(IDirect3DDevice9_SetTexture(d,0,0));REC(IDirect3DSurface9_Release(s));REC(IDirect3DTexture9_Release(t));REC(IDirect3DSurface9_Release(back));ok(IDirect3DDevice9_Present(d,0,0,0,0));REC(IDirect3DDevice9_Release(d));REC(IDirect3D9_Release(f));
 {volatile DWORD guard=0x12345678u;DWORD i;for(i=0;i<50000;i++){guard^=i;guard=guard*1664525u+1013904223u;}REC(guard);}
 save();DestroyWindow(w);ExitProcess(0);
}
