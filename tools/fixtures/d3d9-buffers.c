/* Authored SDK buffer lifetime and drawing oracle; hidden, offscreen, silent. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p, int byte, unsigned int n) {
    BYTE *out = p;
    while (n--) *out++ = (BYTE)byte;
    return p;
}
static DWORD words[2048], used;
static void record(DWORD value) { if (used == 2048) ExitProcess(89); words[used++] = value; }
static void save(void) {
    DWORD wrote;
    HANDLE file = CreateFileA("d3d9-buffers.bin", GENERIC_WRITE, 0, NULL,
        CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, words, used*4, &wrote, NULL) || wrote != used*4) ExitProcess(90);
    CloseHandle(file);
}
static void success(HRESULT hr) { record(hr); if (FAILED(hr)) { save(); ExitProcess(91); } }
static DWORD device_refs(IDirect3DDevice9 *d) { DWORD n = IDirect3DDevice9_AddRef(d); IDirect3DDevice9_Release(d); return n-1; }
typedef struct Vertex {float x,y,z,rhw; DWORD color;} Vertex;
static const Vertex quad[4] = {
    {-0.5f,-0.5f,0,1,0xff123456u}, {15.5f,-0.5f,0,1,0xff123456u},
    {-0.5f,11.5f,0,1,0xff123456u}, {15.5f,11.5f,0,1,0xff123456u}
};
static void vertices(void *memory, UINT offset, UINT stride, UINT first) {
    UINT i;
    for(i=0;i!=4;++i) {
        Vertex *out=(Vertex *)((BYTE *)memory+offset+(first+i)*stride);
        out->x=quad[i].x;out->y=quad[i].y;out->z=quad[i].z;out->rhw=quad[i].rhw;out->color=quad[i].color;
    }
}
static void capture(IDirect3DDevice9 *device,HANDLE file) {
    IDirect3DSurface9 *back=NULL,*readback=NULL;
    D3DLOCKED_RECT locked;
    DWORD pixels[16*12],wrote;
    UINT x,y;
    success(IDirect3DDevice9_GetBackBuffer(device,0,0,D3DBACKBUFFER_TYPE_MONO,&back));
    success(IDirect3DDevice9_CreateOffscreenPlainSurface(device,16,12,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&readback,NULL));
    success(IDirect3DDevice9_GetRenderTargetData(device,back,readback));
    success(IDirect3DSurface9_LockRect(readback,&locked,NULL,D3DLOCK_READONLY));
    for(y=0;y!=12;++y)for(x=0;x!=16;++x) pixels[y*16+x]=*((DWORD *)((BYTE *)locked.pBits+y*locked.Pitch)+x);
    if(!WriteFile(file,pixels,sizeof(pixels),&wrote,NULL)||wrote!=sizeof(pixels))ExitProcess(92);
    success(IDirect3DSurface9_UnlockRect(readback));
    record(IDirect3DSurface9_Release(readback));record(IDirect3DSurface9_Release(back));
}
static void drawing(IDirect3DDevice9 *device) {
    IDirect3DVertexBuffer9 *vb=NULL,*got=NULL;
    IDirect3DIndexBuffer9 *ib=NULL,*got_i=NULL;
    void *data=NULL;
    UINT i,step,offset,stride;
    HANDLE file=CreateFileA("d3d9-buffers-pixels.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
    if(file==INVALID_HANDLE_VALUE)ExitProcess(93);
    success(IDirect3DDevice9_CreateVertexBuffer(device,256,0,0,D3DPOOL_MANAGED,&vb,NULL));
    success(IDirect3DDevice9_CreateIndexBuffer(device,32,0,D3DFMT_INDEX16,D3DPOOL_MANAGED,&ib,NULL));
    success(IDirect3DDevice9_SetFVF(device,D3DFVF_XYZRHW|D3DFVF_DIFFUSE));
    success(IDirect3DDevice9_SetRenderState(device,D3DRS_LIGHTING,FALSE));
    success(IDirect3DDevice9_SetRenderState(device,D3DRS_CULLMODE,D3DCULL_NONE));
    for(step=0;step!=7;++step) {
        const WORD list[6]={0,1,2,2,1,3};
        Vertex up[6];
        WORD user_indices[6]={2,3,4,4,3,5};
        success(IDirect3DVertexBuffer9_Lock(vb,0,0,&data,0));
        vertices(data,16,24,step==0?1:step==2?0:2);
        success(IDirect3DVertexBuffer9_Unlock(vb));
        success(IDirect3DDevice9_SetStreamSource(device,0,vb,16,24));
        if(step==3) {
            record(IDirect3DIndexBuffer9_Release(ib));
            success(IDirect3DDevice9_CreateIndexBuffer(device,32,0,D3DFMT_INDEX32,D3DPOOL_MANAGED,&ib,NULL));
        }
        success(IDirect3DIndexBuffer9_Lock(ib,0,0,&data,0));
        if(step==3) {DWORD *v=data;v[0]=0xffffffffu;for(i=0;i!=6;++i)v[i+1]=list[i]+1;}
        else {WORD *v=data;v[0]=0xffff;for(i=0;i!=6;++i)v[i+1]=list[i]+(step==2?1:0);}
        success(IDirect3DIndexBuffer9_Unlock(ib));
        success(IDirect3DDevice9_SetIndices(device,ib));
        success(IDirect3DDevice9_Clear(device,0,NULL,D3DCLEAR_TARGET,0xff010203u,1,0));
        success(IDirect3DDevice9_BeginScene(device));
        if(step==0) success(IDirect3DDevice9_DrawPrimitive(device,D3DPT_TRIANGLESTRIP,1,2));
        else if(step==1) success(IDirect3DDevice9_DrawIndexedPrimitive(device,D3DPT_TRIANGLELIST,2,0,4,1,2));
        else if(step==2) success(IDirect3DDevice9_DrawIndexedPrimitive(device,D3DPT_TRIANGLELIST,-1,1,4,1,2));
        else if(step==3) success(IDirect3DDevice9_DrawIndexedPrimitive(device,D3DPT_TRIANGLELIST,1,1,4,1,2));
        else if(step==4) {
            vertices(up,0,sizeof(Vertex),2);
            success(IDirect3DDevice9_DrawIndexedPrimitiveUP(device,D3DPT_TRIANGLELIST,2,4,2,user_indices,D3DFMT_INDEX16,up,sizeof(Vertex)));
        } else if(step==5) success(IDirect3DDevice9_DrawPrimitiveUP(device,D3DPT_TRIANGLESTRIP,2,quad,sizeof(Vertex)));
        else {
            BYTE *source=VirtualAlloc(NULL,8192,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE);
            DWORD old;
            WORD tail[6]={205,206,207,207,206,208};
            if(!source)ExitProcess(95);
            vertices(source,0,sizeof(Vertex),205);
            if(!VirtualProtect(source,4096,PAGE_NOACCESS,&old))ExitProcess(96);
            success(IDirect3DDevice9_DrawIndexedPrimitiveUP(device,D3DPT_TRIANGLELIST,205,4,2,tail,D3DFMT_INDEX16,source,sizeof(Vertex)));
            if(!VirtualFree(source,0,MEM_RELEASE))ExitProcess(97);
        }
        success(IDirect3DDevice9_EndScene(device));
        success(IDirect3DDevice9_GetStreamSource(device,0,&got,&offset,&stride));
        record(got==NULL);record(offset);record(stride);if(got)record(IDirect3DVertexBuffer9_Release(got));else record(0);
        success(IDirect3DDevice9_GetIndices(device,&got_i));record(got_i==NULL);if(got_i)record(IDirect3DIndexBuffer9_Release(got_i));else record(0);
        capture(device,file);
    }
    CloseHandle(file);
    record(IDirect3DVertexBuffer9_Release(vb));record(IDirect3DIndexBuffer9_Release(ib));
    success(IDirect3DDevice9_SetStreamSource(device,0,NULL,0,0));success(IDirect3DDevice9_SetIndices(device,NULL));
}
static void zero_draws(IDirect3DDevice9 *device) {
    IDirect3DVertexBuffer9 *vb=NULL,*got=NULL;
    IDirect3DIndexBuffer9 *ib=NULL,*got_i=NULL;
    UINT method,offset,stride;
    WORD indices[3]={0,0,0};
    success(IDirect3DDevice9_CreateVertexBuffer(device,128,0,0,D3DPOOL_MANAGED,&vb,NULL));
    success(IDirect3DDevice9_CreateIndexBuffer(device,32,0,D3DFMT_INDEX16,D3DPOOL_MANAGED,&ib,NULL));
    success(IDirect3DDevice9_SetFVF(device,D3DFVF_XYZRHW|D3DFVF_DIFFUSE));
    for(method=0;method!=6;++method) {
        success(IDirect3DDevice9_SetStreamSource(device,0,vb,4,20));
        success(IDirect3DDevice9_SetIndices(device,ib));
        success(IDirect3DDevice9_BeginScene(device));
        record(0x0dad0000u+method);
        if(method==0)record(IDirect3DDevice9_DrawPrimitive(device,D3DPT_TRIANGLELIST,0,0));
        else if(method==1)record(IDirect3DDevice9_DrawIndexedPrimitive(device,D3DPT_TRIANGLELIST,0,0,0,0,0));
        else if(method==2)record(IDirect3DDevice9_DrawPrimitiveUP(device,D3DPT_TRIANGLELIST,0,quad,sizeof(Vertex)));
        else if(method==3)record(IDirect3DDevice9_DrawIndexedPrimitiveUP(device,D3DPT_TRIANGLELIST,0,0,0,indices,D3DFMT_INDEX16,quad,sizeof(Vertex)));
        else if(method==4)record(IDirect3DDevice9_DrawPrimitiveUP(device,D3DPT_TRIANGLELIST,0,NULL,sizeof(Vertex)));
        else record(IDirect3DDevice9_DrawIndexedPrimitiveUP(device,D3DPT_TRIANGLELIST,0,0,0,NULL,D3DFMT_INDEX16,NULL,sizeof(Vertex)));
        success(IDirect3DDevice9_EndScene(device));
        success(IDirect3DDevice9_GetStreamSource(device,0,&got,&offset,&stride));record(got==NULL);record(offset);record(stride);record(got?IDirect3DVertexBuffer9_Release(got):0);
        success(IDirect3DDevice9_GetIndices(device,&got_i));record(got_i==NULL);record(got_i?IDirect3DIndexBuffer9_Release(got_i):0);
    }
    record(IDirect3DVertexBuffer9_Release(vb));record(IDirect3DIndexBuffer9_Release(ib));
    success(IDirect3DDevice9_SetStreamSource(device,0,NULL,0,0));success(IDirect3DDevice9_SetIndices(device,NULL));
}
void mainCRTStartup(void) {
    HWND window = CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE, "STATIC", "D3D9 buffer oracle", WS_OVERLAPPED,
        -32000, -32000, 64, 64, NULL, NULL, GetModuleHandleA(NULL), NULL);
    IDirect3D9 *factory = Direct3DCreate9(D3D_SDK_VERSION);
    IDirect3DDevice9 *device = NULL;
    D3DPRESENT_PARAMETERS p = {0};
    IDirect3DVertexBuffer9 *vb = NULL, *got = NULL;
    IDirect3DIndexBuffer9 *ib = NULL, *got_i = NULL;
    IDirect3DDevice9 *parent = NULL;
    D3DVERTEXBUFFER_DESC vd;
    D3DINDEXBUFFER_DESC id;
    UINT offset = 0xcccccccc, stride = 0xcccccccc;
    void *base = NULL, *other = NULL;
    UINT i, pool, usage;
    static const DWORD flags[] = {0, D3DLOCK_READONLY, D3DLOCK_DISCARD, D3DLOCK_NOOVERWRITE,
        D3DLOCK_NOSYSLOCK, D3DLOCK_NO_DIRTY_UPDATE, 0x80000000u, D3DLOCK_READONLY|D3DLOCK_DISCARD};
    if (!window || !factory) ExitProcess(1);
    p.BackBufferWidth = 16; p.BackBufferHeight = 12; p.BackBufferFormat = D3DFMT_A8R8G8B8;
    p.BackBufferCount = 1; p.SwapEffect = D3DSWAPEFFECT_DISCARD; p.hDeviceWindow = window;
    p.Windowed = TRUE; p.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    success(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,window,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&device));
    record(device_refs(device));
    for (pool=0; pool!=4; ++pool) for (usage=0; usage!=3; ++usage) {
        DWORD u = usage == 0 ? 0 : usage == 1 ? D3DUSAGE_WRITEONLY : D3DUSAGE_DYNAMIC|D3DUSAGE_WRITEONLY;
        HRESULT hr = IDirect3DDevice9_CreateVertexBuffer(device,128,u,0,(D3DPOOL)pool,&vb,NULL);
        record(hr);
        if (SUCCEEDED(hr)) {
            success(IDirect3DVertexBuffer9_GetDesc(vb,&vd));
            record(vd.Format);record(vd.Type);record(vd.Usage);record(vd.Pool);record(vd.Size);record(vd.FVF);
            for (i=0;i<sizeof(flags)/sizeof(flags[0]);++i) {
                record(flags[i]);hr = IDirect3DVertexBuffer9_Lock(vb,0,0,&base,flags[i]); record(hr);
                record(SUCCEEDED(hr)?IDirect3DVertexBuffer9_Unlock(vb):0xffffffffu);
            }
            record(IDirect3DVertexBuffer9_Release(vb)); vb=NULL;
        }
    }
    success(IDirect3DDevice9_CreateVertexBuffer(device,128,0,D3DFVF_XYZRHW|D3DFVF_DIFFUSE,D3DPOOL_MANAGED,&vb,NULL));
    success(IDirect3DDevice9_CreateIndexBuffer(device,32,0,D3DFMT_INDEX16,D3DPOOL_MANAGED,&ib,NULL));
    record(device_refs(device));
    success(IDirect3DIndexBuffer9_GetDesc(ib,&id));record(id.Format);record(id.Type);record(id.Usage);record(id.Pool);record(id.Size);
    record(IDirect3DVertexBuffer9_GetType(vb)); record(IDirect3DIndexBuffer9_GetType(ib));
    success(IDirect3DVertexBuffer9_GetDevice(vb,&parent));record(parent==device);record(IDirect3DDevice9_Release(parent));
    success(IDirect3DVertexBuffer9_Lock(vb,0,0,&base,0));
    success(IDirect3DVertexBuffer9_Lock(vb,8,0,&other,0));record((BYTE *)other==(BYTE *)base+8);
    record(IDirect3DVertexBuffer9_Unlock(vb)); record(IDirect3DVertexBuffer9_Unlock(vb));record(IDirect3DVertexBuffer9_Unlock(vb));
    record(IDirect3DVertexBuffer9_Lock(vb,128,0,&other,0));record(IDirect3DVertexBuffer9_Unlock(vb));
    record(0xbad10000u);
    record(IDirect3DVertexBuffer9_Lock(vb,129,0,&other,0));record(IDirect3DVertexBuffer9_Unlock(vb));
    record(IDirect3DVertexBuffer9_Lock(vb,120,9,&other,0));record(IDirect3DVertexBuffer9_Unlock(vb));
    success(IDirect3DDevice9_SetStreamSource(device,0,vb,4,20));
    success(IDirect3DDevice9_SetIndices(device,ib));
    record(IDirect3DVertexBuffer9_AddRef(vb));record(IDirect3DVertexBuffer9_Release(vb));
    record(IDirect3DIndexBuffer9_AddRef(ib));record(IDirect3DIndexBuffer9_Release(ib));
    record(IDirect3DVertexBuffer9_Release(vb)); record(IDirect3DIndexBuffer9_Release(ib));record(device_refs(device));
    success(IDirect3DDevice9_GetStreamSource(device,0,&got,&offset,&stride));record(got==vb);record(offset);record(stride);record(device_refs(device));
    success(IDirect3DDevice9_GetIndices(device,&got_i));record(got_i==ib);record(device_refs(device));
    record(IDirect3DVertexBuffer9_Release(got));record(IDirect3DIndexBuffer9_Release(got_i));record(device_refs(device));
    success(IDirect3DDevice9_SetStreamSource(device,0,NULL,123,456));
    success(IDirect3DDevice9_GetStreamSource(device,0,&got,&offset,&stride));record(got==NULL);record(offset);record(stride);
    success(IDirect3DDevice9_SetIndices(device,NULL));
    success(IDirect3DDevice9_GetIndices(device,&got_i));record(got_i==NULL);
    /* Default public resources block Reset, failure keeps the prior binding.
       Zero-public default storage and managed public storage can then reset. */
    success(IDirect3DDevice9_CreateVertexBuffer(device,128,0,0,D3DPOOL_DEFAULT,&vb,NULL));
    success(IDirect3DDevice9_SetStreamSource(device,0,vb,4,20));
    record(IDirect3DDevice9_Reset(device,&p));
    record(IDirect3DDevice9_GetStreamSource(device,0,&got,&offset,&stride));record(got==NULL);record(offset);record(stride);
    record(IDirect3DVertexBuffer9_Release(vb));
    success(IDirect3DDevice9_Reset(device,&p));
    success(IDirect3DDevice9_GetStreamSource(device,0,&got,&offset,&stride));record(got==NULL);record(offset);record(stride);
    success(IDirect3DDevice9_CreateVertexBuffer(device,128,0,0,D3DPOOL_MANAGED,&vb,NULL));
    success(IDirect3DDevice9_SetStreamSource(device,0,vb,8,24));
    success(IDirect3DDevice9_Reset(device,&p));
    success(IDirect3DDevice9_GetStreamSource(device,0,&got,&offset,&stride));record(got==NULL);record(offset);record(stride);
    success(IDirect3DVertexBuffer9_Lock(vb,0,0,&base,0));success(IDirect3DVertexBuffer9_Unlock(vb));record(IDirect3DVertexBuffer9_Release(vb));
    zero_draws(device);drawing(device);
    success(IDirect3DDevice9_Present(device,NULL,NULL,NULL,NULL));
    record(IDirect3DDevice9_Release(device));record(IDirect3D9_Release(factory));
    {volatile DWORD guard=0;for(i=0;i!=50000;++i)guard=guard*1664525u+i;record(guard);}
    save();DestroyWindow(window);ExitProcess(0);
}
