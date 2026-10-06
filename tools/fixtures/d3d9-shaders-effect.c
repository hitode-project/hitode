/* Authored binary Effect COM/pass/constant/state restoration oracle. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include <d3dx9.h>
#include "d3d9-shaders-effect-generated.h"
int _fltused;
void *memset(void *p,int c,unsigned n){unsigned char*q=p;while(n--)*q++=(unsigned char)c;return p;}
static DWORD result[128];static UINT used;static HANDLE pixels;
static void record(DWORD v){if(used>=128)ExitProcess(99);result[used++]=v;}
static void save(void){DWORD wrote;HANDLE f=CreateFileA("d3d9-shaders-effect.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(f==INVALID_HANDLE_VALUE)ExitProcess(90);if(!WriteFile(f,result,used*4,&wrote,NULL)||wrote!=used*4)ExitProcess(91);CloseHandle(f);}
static void ok(HRESULT h){record(h);if(FAILED(h)){save();ExitProcess(94);}}
typedef HRESULT(WINAPI *CreateEffect)(IDirect3DDevice9*,LPCVOID,UINT,const D3DXMACRO*,ID3DXInclude*,DWORD,ID3DXEffectPool*,ID3DXEffect**,ID3DXBuffer**);
struct Vertex{float x,y,z,rhw;DWORD color;};
static const struct Vertex quad[4]={{-.5f,-.5f,0,1,0xffffffffu},{15.5f,-.5f,0,1,0xffffffffu},{-.5f,11.5f,0,1,0xffffffffu},{15.5f,11.5f,0,1,0xffffffffu}};
static void capture(IDirect3DDevice9*d,IDirect3DSurface9*back){IDirect3DSurface9*s=NULL;D3DLOCKED_RECT l;DWORD wrote;UINT y;ok(IDirect3DDevice9_CreateOffscreenPlainSurface(d,16,12,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&s,NULL));ok(IDirect3DDevice9_GetRenderTargetData(d,back,s));ok(IDirect3DSurface9_LockRect(s,&l,NULL,D3DLOCK_READONLY));for(y=0;y<12;++y)if(!WriteFile(pixels,(BYTE*)l.pBits+y*l.Pitch,64,&wrote,NULL)||wrote!=64)ExitProcess(92);ok(IDirect3DSurface9_UnlockRect(s));record(IDirect3DSurface9_Release(s));}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(0,"STATIC","Binary Effect oracle",WS_OVERLAPPED,0,0,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);IDirect3D9*f=Direct3DCreate9(D3D_SDK_VERSION);IDirect3DDevice9*d=NULL,*queried=NULL;IDirect3DSurface9*back=NULL;ID3DXEffect*effect=NULL;ID3DXBuffer*errors=NULL;D3DPRESENT_PARAMETERS p={0};D3DXEFFECT_DESC desc;D3DXPARAMETER_DESC parameter;D3DXTECHNIQUE_DESC technique;D3DXPASS_DESC pass;D3DXHANDLE color,tech,pass_handle;D3DXVECTOR4 value={.5f,.25f,.125f,1},actual;DWORD lighting=0;UINT n=0,i;
 HMODULE library=LoadLibraryA("d3dx9_43.dll");CreateEffect create=(CreateEffect)GetProcAddress(library,"D3DXCreateEffect");if(!w||!f||!create)ExitProcess(1);pixels=CreateFileA("d3d9-shaders-effect-pixels.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(pixels==INVALID_HANDLE_VALUE)ExitProcess(2);
 p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=TRUE;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;
 ok(IDirect3D9_CreateDevice(f,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));ok(IDirect3DDevice9_GetBackBuffer(d,0,0,D3DBACKBUFFER_TYPE_MONO,&back));
 ok(create(d,authored_effect,sizeof(authored_effect),NULL,NULL,0,NULL,&effect,&errors));record(errors==NULL);ok(effect->lpVtbl->GetDesc(effect,&desc));record(desc.Parameters);record(desc.Techniques);record(desc.Functions);
 color=effect->lpVtbl->GetParameterByName(effect,NULL,"tone");record(color!=NULL);record(effect->lpVtbl->GetParameter(effect,NULL,0)==color);ok(effect->lpVtbl->GetParameterDesc(effect,color,&parameter));record(parameter.Class);record(parameter.Type);record(parameter.Rows);record(parameter.Columns);record(parameter.Elements);record(parameter.Annotations);record(parameter.StructMembers);record(parameter.Flags);record(parameter.Bytes);
 tech=effect->lpVtbl->GetTechniqueByName(effect,"Primary");record(tech!=NULL);record(effect->lpVtbl->GetTechnique(effect,0)==tech);ok(effect->lpVtbl->GetTechniqueDesc(effect,tech,&technique));record(technique.Passes);record(technique.Annotations);pass_handle=effect->lpVtbl->GetPass(effect,tech,0);record(pass_handle!=NULL);record(effect->lpVtbl->GetPassByName(effect,tech,"Color")==pass_handle);ok(effect->lpVtbl->GetPassDesc(effect,pass_handle,&pass));record(pass.Annotations);record(pass.pVertexShaderFunction==NULL);record(pass.pPixelShaderFunction!=NULL);
 ok(effect->lpVtbl->SetVector(effect,color,&value));ok(effect->lpVtbl->GetVector(effect,color,&actual));for(i=0;i<4;++i)record(((DWORD*)&actual)[i]);ok(effect->lpVtbl->SetTechnique(effect,tech));record(effect->lpVtbl->GetCurrentTechnique(effect)==tech);ok(effect->lpVtbl->ValidateTechnique(effect,tech));ok(effect->lpVtbl->GetDevice(effect,&queried));record(queried==d);IDirect3DDevice9_Release(queried);
 ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_LIGHTING,FALSE));ok(IDirect3DDevice9_SetRenderState(d,D3DRS_CULLMODE,D3DCULL_NONE));ok(IDirect3DDevice9_Clear(d,0,NULL,D3DCLEAR_TARGET,0xff000000u,1,0));ok(IDirect3DDevice9_BeginScene(d));ok(effect->lpVtbl->Begin(effect,&n,0));record(n);ok(effect->lpVtbl->BeginPass(effect,0));ok(IDirect3DDevice9_DrawPrimitiveUP(d,D3DPT_TRIANGLESTRIP,2,quad,sizeof(struct Vertex)));ok(effect->lpVtbl->EndPass(effect));ok(effect->lpVtbl->End(effect));ok(IDirect3DDevice9_EndScene(d));capture(d,back);
 ok(IDirect3DDevice9_GetRenderState(d,D3DRS_LIGHTING,&lighting));record(lighting);record(effect->lpVtbl->Release(effect));record(IDirect3DSurface9_Release(back));record(IDirect3DDevice9_Release(d));record(IDirect3D9_Release(f));CloseHandle(pixels);DestroyWindow(w);FreeLibrary(library);{volatile DWORD guard=0;for(i=0;i<50000;++i)guard=guard*33u+i;record(guard);}record(0x5adeff01u);save();ExitProcess(0);
}
