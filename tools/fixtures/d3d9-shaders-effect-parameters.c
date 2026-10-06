/* Native binary Effect typed-parameter, dimension, array, and string oracle. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include <d3dx9.h>
#include "d3d9-shaders-effect-parameters-generated.h"
int _fltused;
static DWORD data[1024];static UINT at;
void *memset(void*p,int c,unsigned n){BYTE*q=p;while(n--)*q++=(BYTE)c;return p;}
static void record(DWORD v){data[at++]=v;}
static void dump(void){DWORD written;HANDLE f=CreateFileA("d3d9-shaders-effect-parameters.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);WriteFile(f,data,at*4,&written,NULL);CloseHandle(f);}
static void ok(HRESULT h){if(FAILED(h)){record(h);dump();ExitProcess(1);}}
typedef HRESULT(WINAPI *CreateEffect)(IDirect3DDevice9*,LPCVOID,UINT,const D3DXMACRO*,ID3DXInclude*,DWORD,ID3DXEffectPool*,ID3DXEffect**,ID3DXBuffer**);
void mainCRTStartup(void){HWND w=CreateWindowExA(0,"STATIC","Numeric oracle",WS_OVERLAPPED,0,0,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);IDirect3D9*f=Direct3DCreate9(D3D_SDK_VERSION);IDirect3DDevice9*d;D3DPRESENT_PARAMETERS p={0};ID3DXEffect*e;HMODULE lib=LoadLibraryA("d3dx9_43.dll");CreateEffect create=(CreateEffect)GetProcAddress(lib,"D3DXCreateEffect");D3DXHANDLE h[7];const char*names[]={"f","i","b","v","m","a","label"};UINT j,k;float x;int n;BOOL b;D3DXVECTOR4 v={9,8,7,6},out;D3DXMATRIX m;const char*s;
p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=TRUE;ok(IDirect3D9_CreateDevice(f,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));ok(create(d,authored_parameters,sizeof(authored_parameters),NULL,NULL,0,NULL,&e,NULL));for(j=0;j<7;++j)h[j]=e->lpVtbl->GetParameterByName(e,NULL,names[j]);
for(j=0;j<3;++j){record(100+j);record(e->lpVtbl->GetFloat(e,h[j],&x));record(*(DWORD*)&x);record(e->lpVtbl->GetInt(e,h[j],&n));record(n);record(e->lpVtbl->GetBool(e,h[j],&b));record(b);record(e->lpVtbl->SetInt(e,h[j],-3));record(e->lpVtbl->GetValue(e,h[j],&n,4));record(n);record(e->lpVtbl->SetFloat(e,h[j],2.75f));record(e->lpVtbl->GetValue(e,h[j],&n,4));record(n);record(e->lpVtbl->SetBool(e,h[j],-1));record(e->lpVtbl->GetValue(e,h[j],&n,4));record(n);}
record(200);record(e->lpVtbl->GetVector(e,h[3],&out));for(k=0;k<4;++k)record(((DWORD*)&out)[k]);record(e->lpVtbl->SetVector(e,h[3],&v));record(e->lpVtbl->GetValue(e,h[3],&out,12));for(k=0;k<3;++k)record(((DWORD*)&out)[k]);for(k=0;k<16;++k)((DWORD*)&m)[k]=0xccccccccu;record(300);record(e->lpVtbl->GetMatrix(e,h[4],&m));for(k=0;k<16;++k)record(((DWORD*)&m)[k]);for(k=0;k<16;++k)((DWORD*)&m)[k]=0xccccccccu;record(e->lpVtbl->GetMatrixTranspose(e,h[4],&m));for(k=0;k<16;++k)record(((DWORD*)&m)[k]);for(k=0;k<16;++k)((float*)&m)[k]=(float)(k+10);record(e->lpVtbl->SetMatrix(e,h[4],&m));record(e->lpVtbl->GetValue(e,h[4],&m,24));for(k=0;k<6;++k)record(((DWORD*)&m)[k]);
record(400);record(e->lpVtbl->GetString(e,h[6],&s));record(s&&s[0]=='i'&&s[6]=='l');record(e->lpVtbl->IsParameterUsed(e,h[0],e->lpVtbl->GetTechnique(e,0)));record(e->lpVtbl->SetVector(e,e->lpVtbl->GetParameterElement(e,h[5],1),&v));record(e->lpVtbl->GetValue(e,h[5],&m,32));for(k=0;k<8;++k)record(((DWORD*)&m)[k]);record(e->lpVtbl->GetVector(e,e->lpVtbl->GetParameterElement(e,h[5],0),&out));for(k=0;k<4;++k)record(((DWORD*)&out)[k]);record(e->lpVtbl->Release(e));record(IDirect3DDevice9_Release(d));record(IDirect3D9_Release(f));{volatile DWORD guard=0;for(j=0;j<50000;++j)guard=guard*33u+j;record(guard);}record(0x5adef00fu);dump();DestroyWindow(w);ExitProcess(0);}
