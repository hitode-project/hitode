#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int c,unsigned n){unsigned char *q=p;while(n--)*q++=(unsigned char)c;return p;}
static DWORD results[1024],count;
static void record(DWORD tag,DWORD value){results[count++]=tag;results[count++]=value;}
static void save(void){DWORD wrote;HANDLE file=CreateFileA("d3d9-lockable.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);if(file==INVALID_HANDLE_VALUE||!WriteFile(file,results,count*4,&wrote,0)||wrote!=count*4)ExitProcess(90);CloseHandle(file);}
static void required(DWORD tag,HRESULT value){record(tag,value);if(FAILED(value)){save();ExitProcess(91);}}
static void pixels(IDirect3DSurface9 *surface,DWORD tag){
 D3DLOCKED_RECT lock;UINT x,y;required(tag,IDirect3DSurface9_LockRect(surface,&lock,0,D3DLOCK_READONLY));
 for(y=0;y<6;y++)for(x=0;x<8;x++)record(tag+1+y*8+x,((DWORD*)((BYTE*)lock.pBits+y*lock.Pitch))[x]);
 required(tag+49,IDirect3DSurface9_UnlockRect(surface));
}
struct Vertex {float x,y,z,rhw;DWORD color;};
void mainCRTStartup(void){
 HWND window=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","lockable surface oracle",WS_OVERLAPPED,-32000,-32000,32,32,0,0,GetModuleHandleA(0),0);
 IDirect3D9 *factory=Direct3DCreate9(32);IDirect3DDevice9 *device=0;IDirect3DSurface9 *back=0;
 D3DPRESENT_PARAMETERS p={0};D3DLOCKED_RECT lock,second;RECT area={2,1,5,4};HRESULT result;UINT x,y;DWORD value;
 struct Vertex triangle[3]={{-.5f,-.5f,.5f,1,0xffffffff},{7.5f,-.5f,.5f,1,0xffffffff},{-.5f,5.5f,.5f,1,0xffffffff}};
 if(!window||!factory)ExitProcess(2);
 p.BackBufferWidth=8;p.BackBufferHeight=6;p.BackBufferFormat=21;p.BackBufferCount=1;p.SwapEffect=1;p.hDeviceWindow=window;p.Windowed=1;p.PresentationInterval=0x80000000;
 required(1,IDirect3D9_CreateDevice(factory,0,1,window,0x22,&p,&device));
 required(2,IDirect3DDevice9_GetBackBuffer(device,0,0,0,&back));
 result=IDirect3DSurface9_LockRect(back,&lock,0,0);record(3,result);if(SUCCEEDED(result))IDirect3DSurface9_UnlockRect(back);
 record(4,IDirect3DSurface9_Release(back));record(5,IDirect3DDevice9_Release(device));
 device=0;back=0;p.Flags=D3DPRESENTFLAG_LOCKABLE_BACKBUFFER;
 required(10,IDirect3D9_CreateDevice(factory,0,1,window,0x22,&p,&device));
 required(11,IDirect3DDevice9_GetBackBuffer(device,0,0,0,&back));
 required(12,IDirect3DDevice9_Clear(device,0,0,D3DCLEAR_TARGET,0xff112233,1,0));
 pixels(back,100);
 required(20,IDirect3DSurface9_LockRect(back,&lock,&area,0));
 for(y=0;y<3;y++)for(x=0;x<3;x++)((DWORD*)((BYTE*)lock.pBits+y*lock.Pitch))[x]=0xffa0b0c0;
 required(21,IDirect3DSurface9_UnlockRect(back));pixels(back,200);
 required(30,IDirect3DSurface9_LockRect(back,&lock,0,D3DLOCK_NOSYSLOCK));
 for(y=0;y<6;y++)for(x=0;x<8;x++)((DWORD*)((BYTE*)lock.pBits+y*lock.Pitch))[x]=0xff445566;
 required(31,IDirect3DSurface9_UnlockRect(back));pixels(back,300);
 required(40,IDirect3DSurface9_LockRect(back,&lock,0,D3DLOCK_READONLY|D3DLOCK_NO_DIRTY_UPDATE));
 result=IDirect3DSurface9_LockRect(back,&second,0,D3DLOCK_READONLY);record(41,result);if(SUCCEEDED(result))IDirect3DSurface9_UnlockRect(back);
 required(42,IDirect3DSurface9_UnlockRect(back));record(43,IDirect3DSurface9_UnlockRect(back));
 // Reset must refuse an outstanding public backbuffer reference.
 record(50,IDirect3DDevice9_Reset(device,&p));record(51,IDirect3DSurface9_Release(back));back=0;
 required(52,IDirect3DDevice9_Reset(device,&p));required(53,IDirect3DDevice9_GetBackBuffer(device,0,0,0,&back));
 required(54,IDirect3DDevice9_Clear(device,0,0,D3DCLEAR_TARGET,0xff778899,1,0));pixels(back,400);
 // Observe nonblocking flags separately: readiness is device dependent.
 result=IDirect3DSurface9_LockRect(back,&lock,0,D3DLOCK_READONLY|D3DLOCK_DONOTWAIT);record(60,result);if(SUCCEEDED(result))required(61,IDirect3DSurface9_UnlockRect(back));
 required(70,IDirect3DDevice9_SetFVF(device,D3DFVF_XYZRHW|D3DFVF_DIFFUSE));
 required(71,IDirect3DDevice9_SetRenderState(device,D3DRS_LIGHTING,FALSE));
 required(72,IDirect3DDevice9_SetRenderState(device,D3DRS_CULLMODE,D3DCULL_NONE));
 required(73,IDirect3DSurface9_LockRect(back,&lock,0,D3DLOCK_READONLY));
 record(680,IDirect3DDevice9_SetDialogBoxMode(device,TRUE));
 record(74,IDirect3DDevice9_Present(device,0,0,0,0));
 record(75,IDirect3DDevice9_Clear(device,0,0,D3DCLEAR_TARGET,0xffabcdef,1,0));
 result=IDirect3DDevice9_BeginScene(device);record(76,result);
 if(SUCCEEDED(result)){record(77,IDirect3DDevice9_DrawPrimitiveUP(device,D3DPT_TRIANGLELIST,1,triangle,sizeof(triangle[0])));record(78,IDirect3DDevice9_EndScene(device));}
 required(79,IDirect3DSurface9_UnlockRect(back));
 // Isolate the following raster comparison from operations attempted while
 // locked: those observations do not define retained backbuffer contents.
 record(690,IDirect3DSurface9_Release(back));record(691,IDirect3DDevice9_Release(device));back=0;device=0;
 required(692,IDirect3D9_CreateDevice(factory,0,1,window,0x22,&p,&device));
 required(693,IDirect3DDevice9_GetBackBuffer(device,0,0,0,&back));
 required(694,IDirect3DDevice9_SetFVF(device,D3DFVF_XYZRHW|D3DFVF_DIFFUSE));
 required(695,IDirect3DDevice9_SetRenderState(device,D3DRS_LIGHTING,FALSE));
 required(696,IDirect3DDevice9_SetRenderState(device,D3DRS_CULLMODE,D3DCULL_NONE));
 record(697,IDirect3DDevice9_SetDialogBoxMode(device,FALSE));
 record(698,IDirect3DDevice9_SetDialogBoxMode(device,TRUE));
 required(720,IDirect3DDevice9_BeginScene(device));
 record(721,IDirect3DDevice9_SetDialogBoxMode(device,FALSE));
 record(722,IDirect3DDevice9_SetDialogBoxMode(device,TRUE));
 required(723,IDirect3DDevice9_EndScene(device));
 // No SPECULAR member is supplied by this supported FVF. Specular state must
 // preserve these unsaturated RGB gradients and the diffuse alpha channel.
 required(700,IDirect3DDevice9_GetRenderState(device,D3DRS_SPECULARENABLE,&value));record(701,value);
 triangle[0].color=0x80202020;triangle[1].color=0x80802020;triangle[2].color=0x80208020;
 for(UINT enabled=0;enabled<2;enabled++){
  DWORD tag=702+enabled*8;
  required(tag,IDirect3DDevice9_SetRenderState(device,D3DRS_SPECULARENABLE,enabled));
  required(tag+1,IDirect3DDevice9_GetRenderState(device,D3DRS_SPECULARENABLE,&value));record(tag+2,value);
  required(tag+3,IDirect3DDevice9_Clear(device,0,0,D3DCLEAR_TARGET,0xff112233,1,0));
  required(tag+4,IDirect3DDevice9_BeginScene(device));
  required(tag+5,IDirect3DDevice9_DrawPrimitiveUP(device,D3DPT_TRIANGLELIST,1,triangle,sizeof(triangle[0])));
  required(tag+6,IDirect3DDevice9_EndScene(device));pixels(back,500+enabled*100);
 }
 record(80,IDirect3DSurface9_Release(back));record(81,IDirect3DDevice9_Release(device));record(82,IDirect3D9_Release(factory));
 DestroyWindow(window);record(999,0xd3d9bacc);save();ExitProcess(0);
}
