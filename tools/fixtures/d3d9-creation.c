/* Authored windowed creation/query oracle. No activation, display change or sound. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int v,unsigned n){BYTE *b=p;while(n--)*b++=(BYTE)v;return p;}
static DWORD words[1024],used;
static void record(DWORD n){if(used==1024)ExitProcess(89);words[used++]=n;}
static void save(void){DWORD n;HANDLE f=CreateFileA("d3d9-creation.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(f==INVALID_HANDLE_VALUE||!WriteFile(f,words,used*4,&n,NULL)||n!=used*4)ExitProcess(90);CloseHandle(f);}
static void success(HRESULT hr){record(hr);if(FAILED(hr)){save();ExitProcess(91);}}
typedef struct Vertex{float x,y,z;DWORD color;}Vertex;
static const Vertex quad[4]={{-1,1,0,0xff2468acu},{1,1,0,0xff2468acu},{-1,-1,0,0xff2468acu},{1,-1,0,0xff2468acu}};
static D3DPRESENT_PARAMETERS parameters(HWND window){D3DPRESENT_PARAMETERS p={0};p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=window;p.Windowed=TRUE;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;return p;}
static void creation(IDirect3DDevice9 *d,HWND focus){D3DDEVICE_CREATION_PARAMETERS c={0};success(IDirect3DDevice9_GetCreationParameters(d,&c));record(c.AdapterOrdinal);record(c.DeviceType);record(c.hFocusWindow==focus);record(c.BehaviorFlags);}
static void shaders(IDirect3DDevice9 *d){IDirect3DVertexShader9 *v=(void *)0xdeadbeef;IDirect3DPixelShader9 *p=(void *)0xdeadbeef;success(IDirect3DDevice9_SetVertexShader(d,NULL));success(IDirect3DDevice9_SetPixelShader(d,NULL));success(IDirect3DDevice9_GetVertexShader(d,&v));record(v==NULL);success(IDirect3DDevice9_GetPixelShader(d,&p));record(p==NULL);}
static void capture(IDirect3DDevice9 *d,HANDLE f){IDirect3DSurface9 *back=NULL,*read=NULL;D3DLOCKED_RECT l;DWORD pixels[16*12],n;UINT x,y;success(IDirect3DDevice9_GetBackBuffer(d,0,0,D3DBACKBUFFER_TYPE_MONO,&back));success(IDirect3DDevice9_CreateOffscreenPlainSurface(d,16,12,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&read,NULL));success(IDirect3DDevice9_GetRenderTargetData(d,back,read));success(IDirect3DSurface9_LockRect(read,&l,NULL,D3DLOCK_READONLY));for(y=0;y<12;++y)for(x=0;x<16;++x)pixels[y*16+x]=*((DWORD *)((BYTE *)l.pBits+y*l.Pitch)+x);if(!WriteFile(f,pixels,sizeof(pixels),&n,NULL)||n!=sizeof(pixels))ExitProcess(92);success(IDirect3DSurface9_UnlockRect(read));record(IDirect3DSurface9_Release(read));record(IDirect3DSurface9_Release(back));}
static void buffer_policy(IDirect3D9 *factory,HWND window,DWORD flags,BOOL transformed){
    IDirect3DDevice9 *d=NULL;D3DPRESENT_PARAMETERS p=parameters(window);UINT usage,i,stride=transformed?20:16;DWORD fvf=(transformed?D3DFVF_XYZRHW:D3DFVF_XYZ)|D3DFVF_DIFFUSE;
    record(0xc3000000u|flags);record(flags);record(fvf);success(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,window,flags,&p,&d));success(IDirect3DDevice9_SetFVF(d,fvf));success(IDirect3DDevice9_SetRenderState(d,D3DRS_LIGHTING,FALSE));record(IDirect3DDevice9_SetSoftwareVertexProcessing(d,TRUE));record(IDirect3DDevice9_GetSoftwareVertexProcessing(d));
    for(usage=0;usage!=2;++usage){
        IDirect3DVertexBuffer9 *v=NULL;IDirect3DIndexBuffer9 *b=NULL;void *memory=NULL;HRESULT vh,ih;WORD *indices;
        vh=IDirect3DDevice9_CreateVertexBuffer(d,80,usage?D3DUSAGE_SOFTWAREPROCESSING:0,fvf,D3DPOOL_MANAGED,&v,NULL);ih=IDirect3DDevice9_CreateIndexBuffer(d,12,usage?D3DUSAGE_SOFTWAREPROCESSING:0,D3DFMT_INDEX16,D3DPOOL_MANAGED,&b,NULL);record(vh);record(ih);
        if(v&&b){
            success(IDirect3DVertexBuffer9_Lock(v,0,0,&memory,0));
            for(i=0;i!=4;++i){float *out=(float *)((BYTE *)memory+i*stride);out[0]=transformed?(i&1?15.5f:-0.5f):quad[i].x;out[1]=transformed?(i&2?11.5f:-0.5f):quad[i].y;out[2]=0;if(transformed)out[3]=1;*((DWORD *)((BYTE *)memory+i*stride+stride-4))=0xff2468acu;}
            success(IDirect3DVertexBuffer9_Unlock(v));success(IDirect3DIndexBuffer9_Lock(b,0,0,&memory,0));indices=memory;indices[0]=0;indices[1]=1;indices[2]=2;indices[3]=2;indices[4]=1;indices[5]=3;success(IDirect3DIndexBuffer9_Unlock(b));
            success(IDirect3DDevice9_SetStreamSource(d,0,v,0,stride));success(IDirect3DDevice9_SetIndices(d,b));success(IDirect3DDevice9_BeginScene(d));record(IDirect3DDevice9_DrawPrimitive(d,D3DPT_TRIANGLESTRIP,0,2));record(IDirect3DDevice9_DrawIndexedPrimitive(d,D3DPT_TRIANGLELIST,0,0,4,0,2));
            record(IDirect3DDevice9_SetSoftwareVertexProcessing(d,FALSE));record(IDirect3DDevice9_GetSoftwareVertexProcessing(d));record(IDirect3DDevice9_DrawPrimitive(d,D3DPT_TRIANGLESTRIP,0,2));record(IDirect3DDevice9_DrawIndexedPrimitive(d,D3DPT_TRIANGLELIST,0,0,4,0,2));record(IDirect3DDevice9_SetSoftwareVertexProcessing(d,TRUE));record(IDirect3DDevice9_GetSoftwareVertexProcessing(d));
            success(IDirect3DDevice9_EndScene(d));success(IDirect3DDevice9_SetStreamSource(d,0,NULL,0,0));success(IDirect3DDevice9_SetIndices(d,NULL));record(IDirect3DVertexBuffer9_Release(v));record(IDirect3DIndexBuffer9_Release(b));
        }else{for(i=0;i!=18;++i)record(0xfffffffeu);record(v?IDirect3DVertexBuffer9_Release(v):0xffffffffu);record(b?IDirect3DIndexBuffer9_Release(b):0xffffffffu);}
    }
    record(IDirect3DDevice9_Release(d));
}
void mainCRTStartup(void){
    HWND window=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","authored d3d9 creation",WS_OVERLAPPED,-32000,-32000,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);
    IDirect3D9 *factory=Direct3DCreate9(D3D_SDK_VERSION);IDirect3DDevice9 *device=NULL;
    DWORD modes[6]={0x22,0x42,0x82,0x26,0x46,0x86};DWORD bad[6]={2,0x62,0xa2,0xc2,0xe2,0x32};BOOL values[5]={FALSE,TRUE,-1,2,FALSE};UINT i,j;
    HANDLE pixels=CreateFileA("d3d9-creation-pixels.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
    if(!window||!factory||pixels==INVALID_HANDLE_VALUE)ExitProcess(1);
    for(i=0;i<6;++i){
        HWND focus=i<3?window:NULL;D3DPRESENT_PARAMETERS p=parameters(window);DWORD fvf=0;
        record(0xd3000000u+i);record(modes[i]);success(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,focus,modes[i],&p,&device));creation(device,focus);
        record(IDirect3DDevice9_GetSoftwareVertexProcessing(device));
        for(j=0;j<5;++j){record(values[j]);record(IDirect3DDevice9_SetSoftwareVertexProcessing(device,values[j]));record(IDirect3DDevice9_GetSoftwareVertexProcessing(device));}
        shaders(device);
        success(IDirect3DDevice9_SetFVF(device,D3DFVF_XYZ|D3DFVF_DIFFUSE));shaders(device);success(IDirect3DDevice9_GetFVF(device,&fvf));record(fvf);
        record(IDirect3DDevice9_SetSoftwareVertexProcessing(device,TRUE));record(IDirect3DDevice9_GetSoftwareVertexProcessing(device));
        success(IDirect3DDevice9_SetRenderState(device,D3DRS_LIGHTING,FALSE));success(IDirect3DDevice9_Clear(device,0,NULL,D3DCLEAR_TARGET,0xff000000u,1,0));success(IDirect3DDevice9_BeginScene(device));success(IDirect3DDevice9_DrawPrimitiveUP(device,D3DPT_TRIANGLESTRIP,2,quad,sizeof(Vertex)));success(IDirect3DDevice9_EndScene(device));capture(device,pixels);
        record(IDirect3DDevice9_SetSoftwareVertexProcessing(device,TRUE));record(IDirect3DDevice9_GetSoftwareVertexProcessing(device));p=parameters(window);success(IDirect3DDevice9_Reset(device,&p));record(IDirect3DDevice9_GetSoftwareVertexProcessing(device));creation(device,focus);shaders(device);
        record(0x90000010u);record(IDirect3DDevice9_Present(device,NULL,NULL,NULL,NULL));record(IDirect3DDevice9_Release(device));device=NULL;
    }
    for(i=0;i<6;++i){D3DPRESENT_PARAMETERS p=parameters(window);device=NULL;record(0xbad00000u+i);record(bad[i]);record(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,window,bad[i],&p,&device));record(device==NULL);if(device)IDirect3DDevice9_Release(device);}
    for(i=0;i!=3;++i){buffer_policy(factory,window,modes[i],FALSE);buffer_policy(factory,window,modes[i],TRUE);}
    CloseHandle(pixels);record(IDirect3D9_Release(factory));DestroyWindow(window);
    {volatile DWORD guard=0;for(i=0;i<50000;++i)guard=guard*33u+i;record(guard);}save();ExitProcess(0);
}
