#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void*p,int c,unsigned n){BYTE*q=p;while(n--)*q++=(BYTE)c;return p;}
typedef struct {const char*name;RECT source,destination;} Case;
static const Case cases[]={
 {"full_enlarge",{0,0,4,4},{1,1,7,7}},
 {"enlarge",{0,0,2,2},{1,1,5,5}},
 {"shrink_four_to_three",{0,0,4,4},{2,1,5,4}},
 {"cropped_enlarge",{1,1,3,3},{2,2,6,6}},
};
static HANDLE output;
static void emit(const char*s){DWORD n;WriteFile(output,s,lstrlenA(s),&n,0);}
static void okay(HRESULT hr,const char*where){char b[160];if(FAILED(hr)){wsprintfA(b,"{\"error\":\"%s\",\"hr\":\"%08x\"}\n",where,(UINT)hr);emit(b);ExitProcess(3);}}
#define OK(x) okay((x),#x)
void mainCRTStartup(void){
 HWND w;IDirect3D9*f;IDirect3DDevice9*d=0;IDirect3DSurface9*src=0,*dst=0,*read=0;
 D3DPRESENT_PARAMETERS p={0};D3DLOCKED_RECT locked;HRESULT hr;UINT x,y,i,filter,n;char b[3200];
 DWORD colors[16];UINT levels[]={10,50,100,240},alpha[]={16,64,128,240};
 D3DDEVTYPE device_type=D3DDEVTYPE_HAL;LPWSTR command=GetCommandLineW();
 while(*command){if(command[0]=='-'&&command[1]=='r'&&command[2]=='e'&&command[3]=='f')device_type=D3DDEVTYPE_REF;command++;}
 output=GetStdHandle(STD_OUTPUT_HANDLE);
 for(y=0;y<4;y++)for(x=0;x<4;x++)colors[y*4+x]=(alpha[y]<<24)|((x+y)*35<<16)|(levels[y]<<8)|levels[x];
 w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","authored StretchRect oracle",WS_POPUP,-32000,-32000,8,8,0,0,GetModuleHandleA(0),0);
 f=Direct3DCreate9(D3D_SDK_VERSION);if(!w||!f)ExitProcess(2);
 p.BackBufferWidth=8;p.BackBufferHeight=8;p.BackBufferFormat=D3DFMT_X8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.Windowed=1;p.hDeviceWindow=w;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;
 OK(IDirect3D9_CreateDevice(f,0,device_type,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));
 OK(IDirect3DDevice9_CreateOffscreenPlainSurface(d,4,4,D3DFMT_A8R8G8B8,D3DPOOL_DEFAULT,&src,0));
 OK(IDirect3DDevice9_CreateRenderTarget(d,8,8,D3DFMT_A8R8G8B8,D3DMULTISAMPLE_NONE,0,FALSE,&dst,0));
 OK(IDirect3DDevice9_CreateOffscreenPlainSurface(d,8,8,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&read,0));
 OK(IDirect3DSurface9_LockRect(src,&locked,0,0));
 for(y=0;y<4;y++)for(x=0;x<4;x++)((DWORD*)((BYTE*)locked.pBits+y*locked.Pitch))[x]=colors[y*4+x];
 OK(IDirect3DSurface9_UnlockRect(src));
 n=wsprintfA(b,"{\"op\":\"source\",\"pixels\":[");
 for(i=0;i<16;i++)n+=wsprintfA(b+n,"%s\"%08x\"",i?",":"",colors[i]);
 wsprintfA(b+n,"]}\n");emit(b);
 for(i=0;i<sizeof(cases)/sizeof(cases[0]);i++)for(filter=0;filter<3;filter++){
  const Case*c=&cases[i];OK(IDirect3DDevice9_ColorFill(d,dst,0,0xaa010203));
  OK(IDirect3DDevice9_BeginScene(d));hr=IDirect3DDevice9_StretchRect(d,src,&c->source,dst,&c->destination,(D3DTEXTUREFILTERTYPE)filter);OK(IDirect3DDevice9_EndScene(d));
  n=wsprintfA(b,"{\"op\":\"case\",\"name\":\"%s\",\"filter\":%u,\"hr\":\"%08x\",\"source_rect\":[%d,%d,%d,%d],\"destination_rect\":[%d,%d,%d,%d],\"pixels\":[",c->name,filter,(UINT)hr,c->source.left,c->source.top,c->source.right,c->source.bottom,c->destination.left,c->destination.top,c->destination.right,c->destination.bottom);
  OK(IDirect3DDevice9_GetRenderTargetData(d,dst,read));OK(IDirect3DSurface9_LockRect(read,&locked,0,D3DLOCK_READONLY));
  for(y=0;y<8;y++)for(x=0;x<8;x++)n+=wsprintfA(b+n,"%s\"%08x\"",x||y?",":"",((DWORD*)((BYTE*)locked.pBits+y*locked.Pitch))[x]);
  wsprintfA(b+n,"]}\n");emit(b);OK(IDirect3DSurface9_UnlockRect(read));
 }
 IDirect3DSurface9_Release(read);IDirect3DSurface9_Release(dst);IDirect3DSurface9_Release(src);IDirect3DDevice9_Release(d);IDirect3D9_Release(f);DestroyWindow(w);ExitProcess(0);
}
