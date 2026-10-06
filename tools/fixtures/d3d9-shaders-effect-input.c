/* Generic local FXO acceptance harness. Input bytes are read at runtime only.
 * The fixture contains no sample data and writes only HRESULT/count evidence. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include <d3dx9.h>
int _fltused;
void *memset(void*p,int c,unsigned n){unsigned char*q=p;while(n--)*q++=(unsigned char)c;return p;}
typedef HRESULT(WINAPI *CreateEffect)(IDirect3DDevice9*,LPCVOID,UINT,const D3DXMACRO*,ID3DXInclude*,DWORD,ID3DXEffectPool*,ID3DXEffect**,ID3DXBuffer**);
static DWORD result[4096];static UINT used;
static void record(DWORD n){if(used>=4096)ExitProcess(99);result[used++]=n;}
static void save(void){DWORD wrote;HANDLE file=CreateFileA("effect-input-result.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(file==INVALID_HANDLE_VALUE)ExitProcess(90);if(!WriteFile(file,result,used*4,&wrote,NULL)||wrote!=used*4)ExitProcess(91);CloseHandle(file);}
static void ok(HRESULT h){record(h);if(FAILED(h)){save();ExitProcess(94);}}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(0,"STATIC","Local Effect acceptance",WS_OVERLAPPED,0,0,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);IDirect3D9*f=Direct3DCreate9(D3D_SDK_VERSION);IDirect3DDevice9*d=NULL;ID3DXEffect*effect=NULL;ID3DXBuffer*errors=NULL;D3DPRESENT_PARAMETERS p={0};D3DXEFFECT_DESC description;D3DXTECHNIQUE_DESC technique;D3DXHANDLE handle;UINT t,pass,n;DWORD length,read;BYTE*bytes;HANDLE input;HMODULE library=LoadLibraryA("d3dx9_43.dll");CreateEffect create=(CreateEffect)GetProcAddress(library,"D3DXCreateEffect");
 if(!w||!f||!create)ExitProcess(1);input=CreateFileA("effect-input.fxo",GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,FILE_ATTRIBUTE_NORMAL,NULL);if(input==INVALID_HANDLE_VALUE)ExitProcess(2);length=GetFileSize(input,NULL);if(length<24||length>16*1024*1024)ExitProcess(3);bytes=VirtualAlloc(NULL,length,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE);if(!bytes||!ReadFile(input,bytes,length,&read,NULL)||read!=length)ExitProcess(4);CloseHandle(input);
 p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=TRUE;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;
 ok(IDirect3D9_CreateDevice(f,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));
 ok(create(d,bytes,length,NULL,NULL,0,NULL,&effect,&errors));record(effect!=NULL);record(errors==NULL);if(!effect)ExitProcess(5);ok(effect->lpVtbl->GetDesc(effect,&description));record(description.Parameters);record(description.Techniques);
 for(t=0;t<description.Techniques;++t){handle=effect->lpVtbl->GetTechnique(effect,t);record(handle!=NULL);if(!handle)ExitProcess(6);ok(effect->lpVtbl->GetTechniqueDesc(effect,handle,&technique));record(technique.Passes);ok(effect->lpVtbl->SetTechnique(effect,handle));ok(effect->lpVtbl->ValidateTechnique(effect,handle));n=0;ok(effect->lpVtbl->Begin(effect,&n,D3DXFX_DONOTSAVESTATE));record(n);for(pass=0;pass<n;++pass){ok(effect->lpVtbl->BeginPass(effect,pass));ok(effect->lpVtbl->CommitChanges(effect));ok(effect->lpVtbl->EndPass(effect));}ok(effect->lpVtbl->End(effect));}
 record(effect->lpVtbl->Release(effect));record(IDirect3DDevice9_Release(d));record(IDirect3D9_Release(f));VirtualFree(bytes,0,MEM_RELEASE);DestroyWindow(w);FreeLibrary(library);{volatile DWORD guard=0;for(t=0;t<50000;++t)guard=guard*33u+t;record(guard);}record(0x5adef00du);save();ExitProcess(0);
}
