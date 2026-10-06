/* Repository-owned COM/state oracle. No game shaders or Effect files. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int c,unsigned n){unsigned char *q=p;while(n--)*q++=(unsigned char)c;return p;}
static DWORD words[256];static UINT count;
static volatile DWORD execution_probe;
static const DWORD vs[]={0xfffe0200,0x0200001f,0x80000000,0x900f0000,0x02000001,0xc00f0000,0x90e40000,0xffff};
static void save(void){DWORD wrote;HANDLE f=CreateFileA("d3d9-shader-state.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(f==INVALID_HANDLE_VALUE)ExitProcess(90);if(!WriteFile(f,words,count*4,&wrote,NULL)||wrote!=count*4)ExitProcess(91);CloseHandle(f);}
static void record(DWORD x){if(count==256)ExitProcess(92);words[count++]=x;}
static void good(HRESULT hr){record(hr);if(FAILED(hr)){save();ExitProcess(93);}}
static void floats(IDirect3DDevice9 *d,UINT start){float data[4];UINT i;good(IDirect3DDevice9_GetPixelShaderConstantF(d,start,data,1));for(i=0;i<4;i++)record(((DWORD *)data)[i]);}
static D3DPRESENT_PARAMETERS parameters(HWND w){D3DPRESENT_PARAMETERS p={0};p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=TRUE;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;return p;}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(0,"STATIC","D3D9 state oracle",WS_OVERLAPPED,0,0,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);
 IDirect3D9 *factory=Direct3DCreate9(D3D_SDK_VERSION);IDirect3DDevice9 *d=NULL;IDirect3DVertexShader9 *shader=NULL,*queried=NULL;IDirect3DStateBlock9 *block=NULL;
 D3DPRESENT_PARAMETERS p=parameters(w);BOOL booleans[4]={0,2,-1,(BOOL)0x80000000},copied_bool[4];DWORD copied[64],value;UINT i,bytes;
 float original[4]={1,2,3,4},changed[4]={5,6,7,8},unrelated[4]={9,10,11,12},captured[4]={13,14,15,16};
 for(i=0;i<4096;i++)execution_probe+=i;
 if(!w||!factory)ExitProcess(1);good(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));
 good(IDirect3DDevice9_SetVertexShaderConstantB(d,0,booleans,4));good(IDirect3DDevice9_GetVertexShaderConstantB(d,0,copied_bool,4));for(i=0;i<4;i++)record(copied_bool[i]);
 record(IDirect3DDevice9_SetVertexShaderConstantF(d,0xffffffff,(float *)booleans,0));record(IDirect3DDevice9_GetVertexShaderConstantF(d,0xffffffff,NULL,0));
 good(IDirect3DDevice9_SetVertexShaderConstantF(d,0,NULL,0));good(IDirect3DDevice9_GetVertexShaderConstantF(d,0,NULL,0));
 good(IDirect3DDevice9_CreateVertexShader(d,vs,&shader));bytes=4;good(IDirect3DVertexShader9_GetFunction(shader,copied,&bytes));record(bytes);for(i=0;i<bytes/4;i++)record(copied[i]);
 good(IDirect3DDevice9_SetVertexShader(d,shader));record(IDirect3DVertexShader9_Release(shader));
 good(IDirect3DDevice9_CreateStateBlock(d,D3DSBT_ALL,&block));good(IDirect3DDevice9_SetVertexShader(d,NULL));good(IDirect3DDevice9_GetVertexShader(d,&queried));record(queried==NULL);
 good(IDirect3DStateBlock9_Apply(block));good(IDirect3DDevice9_GetVertexShader(d,&queried));record(queried==shader);record(IDirect3DVertexShader9_Release(queried));record(IDirect3DStateBlock9_Release(block));block=NULL;good(IDirect3DDevice9_SetVertexShader(d,NULL));shader=NULL;
 {
  DWORD states[5]={D3DRS_SPECULARENABLE,D3DRS_LIGHTING,D3DRS_CULLMODE,D3DRS_ALPHABLENDENABLE,D3DRS_DIFFUSEMATERIALSOURCE};
  DWORD before[5]={0,1,3,0,1},after[5]={1,0,1,1,2};UINT kind,k;
  for(kind=D3DSBT_PIXELSTATE;kind<=D3DSBT_VERTEXSTATE;kind++){
   for(k=0;k<5;k++)good(IDirect3DDevice9_SetRenderState(d,(D3DRENDERSTATETYPE)states[k],before[k]));good(IDirect3DDevice9_CreateStateBlock(d,(D3DSTATEBLOCKTYPE)kind,&block));
   for(k=0;k<5;k++)good(IDirect3DDevice9_SetRenderState(d,(D3DRENDERSTATETYPE)states[k],after[k]));good(IDirect3DStateBlock9_Apply(block));
   for(k=0;k<5;k++){good(IDirect3DDevice9_GetRenderState(d,(D3DRENDERSTATETYPE)states[k],&value));record(value);}record(IDirect3DStateBlock9_Release(block));block=NULL;
  }
 }
 good(IDirect3DDevice9_SetPixelShaderConstantF(d,7,original,1));good(IDirect3DDevice9_BeginStateBlock(d));good(IDirect3DDevice9_SetPixelShaderConstantF(d,7,changed,1));floats(d,7);good(IDirect3DDevice9_EndStateBlock(d,&block));
 good(IDirect3DDevice9_SetPixelShaderConstantF(d,8,unrelated,1));good(IDirect3DStateBlock9_Apply(block));floats(d,7);floats(d,8);
 good(IDirect3DDevice9_SetPixelShaderConstantF(d,7,captured,1));good(IDirect3DStateBlock9_Capture(block));good(IDirect3DDevice9_SetPixelShaderConstantF(d,7,original,1));good(IDirect3DDevice9_SetPixelShaderConstantF(d,8,changed,1));good(IDirect3DStateBlock9_Apply(block));floats(d,7);floats(d,8);record(IDirect3DStateBlock9_Release(block));block=NULL;
 good(IDirect3DDevice9_CreateVertexShader(d,vs,&shader));good(IDirect3DDevice9_CreateStateBlock(d,D3DSBT_ALL,&block));p=parameters(w);record(IDirect3DDevice9_Reset(d,&p));record(IDirect3DStateBlock9_Release(block));block=NULL;
 p=parameters(w);good(IDirect3DDevice9_Reset(d,&p));bytes=0;good(IDirect3DVertexShader9_GetFunction(shader,NULL,&bytes));record(bytes);record(IDirect3DVertexShader9_Release(shader));record(IDirect3DDevice9_Release(d));record(IDirect3D9_Release(factory));DestroyWindow(w);record(0x5a7e0001);save();ExitProcess(0);
}
