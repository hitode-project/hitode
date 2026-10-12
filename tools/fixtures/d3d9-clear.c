/* Authored viewport Clear oracle: own hidden NOACTIVATE window, no audio. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int v,unsigned n){BYTE *b=p;while(n--)*b++=(BYTE)v;return p;}
static DWORD words[512],used;
static HANDLE pixel_file;
static void record(DWORD n){if(used==512)ExitProcess(89);words[used++]=n;}
static void save(void){DWORD n;HANDLE f=CreateFileA("d3d9-clear.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(f==INVALID_HANDLE_VALUE||!WriteFile(f,words,used*4,&n,NULL)||n!=used*4)ExitProcess(90);CloseHandle(f);}
static void ok(HRESULT hr){record(hr);if(FAILED(hr)){save();ExitProcess(91);}}
static void state(IDirect3DDevice9 *d,D3DRENDERSTATETYPE s,DWORD v){ok(IDirect3DDevice9_SetRenderState(d,s,v));}
static void viewport(IDirect3DDevice9 *d,UINT x,UINT y,UINT w,UINT h,float min,float max){D3DVIEWPORT9 v={x,y,w,h,min,max};ok(IDirect3DDevice9_SetViewport(d,&v));}
static void scissor_value(IDirect3DDevice9 *d){RECT r={-1,-1,-1,-1};ok(IDirect3DDevice9_GetScissorRect(d,&r));record(r.left);record(r.top);record(r.right);record(r.bottom);}
typedef struct Vertex{float x,y,z,rhw;DWORD color;}Vertex;
typedef struct TextureVertex{float x,y,z,rhw;DWORD color;float u,v;}TextureVertex;
static void quad(IDirect3DDevice9 *d,float z,DWORD color){Vertex v[4]={{-.5f,-.5f,z,1,color},{15.5f,-.5f,z,1,color},{-.5f,11.5f,z,1,color},{15.5f,11.5f,z,1,color}};ok(IDirect3DDevice9_BeginScene(d));ok(IDirect3DDevice9_DrawPrimitiveUP(d,D3DPT_TRIANGLESTRIP,2,v,sizeof(Vertex)));ok(IDirect3DDevice9_EndScene(d));}
static void texture_quad(IDirect3DDevice9 *d){TextureVertex v[4]={{-.5f,-.5f,0,1,0xffffffffu,0,0},{15.5f,-.5f,0,1,0xffffffffu,1,0},{-.5f,11.5f,0,1,0xffffffffu,0,1},{15.5f,11.5f,0,1,0xffffffffu,1,1}};ok(IDirect3DDevice9_BeginScene(d));ok(IDirect3DDevice9_DrawPrimitiveUP(d,D3DPT_TRIANGLESTRIP,2,v,sizeof(TextureVertex)));ok(IDirect3DDevice9_EndScene(d));}
static void capture(IDirect3DDevice9 *d,IDirect3DSurface9 *source,D3DFORMAT format){IDirect3DSurface9 *read=NULL;D3DLOCKED_RECT l;DWORD pixels[16*12],n;UINT x,y;ok(IDirect3DDevice9_CreateOffscreenPlainSurface(d,16,12,format,D3DPOOL_SYSTEMMEM,&read,NULL));ok(IDirect3DDevice9_GetRenderTargetData(d,source,read));ok(IDirect3DSurface9_LockRect(read,&l,NULL,D3DLOCK_READONLY));for(y=0;y!=12;++y)for(x=0;x!=16;++x)pixels[y*16+x]=*((DWORD *)((BYTE *)l.pBits+y*l.Pitch)+x);if(!WriteFile(pixel_file,pixels,sizeof(pixels),&n,NULL)||n!=sizeof(pixels))ExitProcess(92);ok(IDirect3DSurface9_UnlockRect(read));record(IDirect3DSurface9_Release(read));}
static void black(IDirect3DDevice9 *d){ok(IDirect3DDevice9_Clear(d,0,NULL,D3DCLEAR_TARGET,0xff000000u,0,0));}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","viewport clear oracle",WS_OVERLAPPED,-32000,-32000,64,64,NULL,NULL,GetModuleHandleA(NULL),NULL);
 IDirect3D9 *factory=Direct3DCreate9(D3D_SDK_VERSION);IDirect3DDevice9 *d=NULL;IDirect3DSurface9 *back=NULL,*x8=NULL;IDirect3DTexture9 *texture=NULL;D3DPRESENT_PARAMETERS p={0};RECT scissor={15,11,16,12},overlap={0,0,16,6};UINT i;
 if(!w||!factory)ExitProcess(1);pixel_file=CreateFileA("d3d9-clear-pixels.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(pixel_file==INVALID_HANDLE_VALUE)ExitProcess(2);
 p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=TRUE;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;p.EnableAutoDepthStencil=TRUE;p.AutoDepthStencilFormat=D3DFMT_D24S8;
 ok(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));ok(IDirect3DDevice9_GetBackBuffer(d,0,0,D3DBACKBUFFER_TYPE_MONO,&back));ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE));state(d,D3DRS_LIGHTING,FALSE);
 ok(IDirect3DDevice9_Clear(d,0,NULL,7,0xffff0000u,1,0));
 viewport(d,0,0,8,12,.8f,.9f);
 state(d,D3DRS_COLORWRITEENABLE,0);state(d,D3DRS_CULLMODE,D3DCULL_CCW);state(d,D3DRS_ALPHABLENDENABLE,TRUE);state(d,D3DRS_SRCBLEND,D3DBLEND_ONE);state(d,D3DRS_DESTBLEND,D3DBLEND_ONE);state(d,D3DRS_ALPHATESTENABLE,TRUE);state(d,D3DRS_ALPHAFUNC,D3DCMP_NEVER);state(d,D3DRS_ZWRITEENABLE,FALSE);state(d,D3DRS_ZFUNC,D3DCMP_NEVER);state(d,D3DRS_STENCILWRITEMASK,0);state(d,D3DRS_SCISSORTESTENABLE,FALSE);ok(IDirect3DDevice9_SetScissorRect(d,&scissor));
 ok(IDirect3DDevice9_Clear(d,0,NULL,7,0xff00ff00u,.25f,7));
 viewport(d,0,0,16,12,0,1);state(d,D3DRS_COLORWRITEENABLE,15);state(d,D3DRS_CULLMODE,D3DCULL_NONE);state(d,D3DRS_ALPHABLENDENABLE,FALSE);state(d,D3DRS_ALPHATESTENABLE,FALSE);state(d,D3DRS_SCISSORTESTENABLE,FALSE);state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILENABLE,FALSE);state(d,D3DRS_STENCILWRITEMASK,0xff);capture(d,back,D3DFMT_A8R8G8B8);
 black(d);state(d,D3DRS_ZENABLE,TRUE);state(d,D3DRS_ZFUNC,D3DCMP_LESS);quad(d,.5f,0xff0000ffu);capture(d,back,D3DFMT_A8R8G8B8);
 black(d);state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILENABLE,TRUE);state(d,D3DRS_STENCILFUNC,D3DCMP_EQUAL);state(d,D3DRS_STENCILPASS,D3DSTENCILOP_KEEP);state(d,D3DRS_STENCILMASK,0xff);state(d,D3DRS_STENCILREF,7);quad(d,.5f,0xff00ffffu);capture(d,back,D3DFMT_A8R8G8B8);
 state(d,D3DRS_STENCILREF,0);quad(d,.5f,0xffffff00u);capture(d,back,D3DFMT_A8R8G8B8);
 viewport(d,0,0,16,6,.8f,.9f);ok(IDirect3DDevice9_Clear(d,0,NULL,D3DCLEAR_STENCIL,0xffabcdefu,.9f,11));viewport(d,0,0,16,12,0,1);black(d);state(d,D3DRS_STENCILREF,11);quad(d,.5f,0xff00ff00u);capture(d,back,D3DFMT_A8R8G8B8);
 black(d);state(d,D3DRS_STENCILENABLE,FALSE);state(d,D3DRS_ZENABLE,TRUE);quad(d,.5f,0xffff0000u);capture(d,back,D3DFMT_A8R8G8B8);
 viewport(d,0,0,16,6,.8f,.9f);ok(IDirect3DDevice9_Clear(d,0,NULL,D3DCLEAR_ZBUFFER,0xffabcdefu,.75f,55));viewport(d,0,0,16,12,0,1);black(d);quad(d,.5f,0xff0000ffu);capture(d,back,D3DFMT_A8R8G8B8);
 black(d);state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILENABLE,TRUE);state(d,D3DRS_STENCILREF,11);quad(d,.5f,0xffff00ffu);capture(d,back,D3DFMT_A8R8G8B8);
 viewport(d,0,0,8,12,.8f,.9f);ok(IDirect3DDevice9_SetScissorRect(d,&overlap));state(d,D3DRS_SCISSORTESTENABLE,TRUE);ok(IDirect3DDevice9_Clear(d,0,NULL,7,0xff112233u,.25f,5));viewport(d,0,0,16,12,0,1);state(d,D3DRS_SCISSORTESTENABLE,FALSE);capture(d,back,D3DFMT_A8R8G8B8);
 viewport(d,0,0,8,12,.8f,.9f);ok(IDirect3DDevice9_SetScissorRect(d,&scissor));state(d,D3DRS_SCISSORTESTENABLE,TRUE);ok(IDirect3DDevice9_Clear(d,0,NULL,7,0xff00ff00u,0,99));viewport(d,0,0,16,12,0,1);state(d,D3DRS_SCISSORTESTENABLE,FALSE);capture(d,back,D3DFMT_A8R8G8B8);
 black(d);state(d,D3DRS_ZENABLE,TRUE);state(d,D3DRS_ZFUNC,D3DCMP_LESS);state(d,D3DRS_STENCILENABLE,FALSE);quad(d,.5f,0xff0000ffu);capture(d,back,D3DFMT_A8R8G8B8);
 black(d);state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILENABLE,TRUE);state(d,D3DRS_STENCILREF,5);quad(d,.5f,0xff00ff00u);capture(d,back,D3DFMT_A8R8G8B8);
 ok(IDirect3DDevice9_CreateTexture(d,16,12,1,D3DUSAGE_RENDERTARGET,D3DFMT_X8R8G8B8,D3DPOOL_DEFAULT,&texture,NULL));ok(IDirect3DTexture9_GetSurfaceLevel(texture,0,&x8));ok(IDirect3DDevice9_SetScissorRect(d,&scissor));ok(IDirect3DDevice9_SetRenderTarget(d,0,x8));scissor_value(d);ok(IDirect3DDevice9_Clear(d,0,NULL,D3DCLEAR_TARGET,0x00112233u,0,0));viewport(d,0,0,8,12,.8f,.9f);ok(IDirect3DDevice9_Clear(d,0,NULL,D3DCLEAR_TARGET,0x00445566u,0,0));
 ok(IDirect3DDevice9_SetRenderTarget(d,0,back));ok(IDirect3DDevice9_SetTexture(d,0,(IDirect3DBaseTexture9 *)texture));ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE|D3DFVF_TEX1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLOROP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLORARG1,D3DTA_TEXTURE));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAOP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAARG1,D3DTA_TEXTURE));state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILENABLE,FALSE);texture_quad(d);capture(d,back,D3DFMT_A8R8G8B8);
 ok(IDirect3DDevice9_SetScissorRect(d,&scissor));ok(IDirect3DDevice9_SetRenderTarget(d,0,back));scissor_value(d);ok(IDirect3DDevice9_SetTexture(d,0,NULL));
 // A smaller color target must not resize or clear outside the original Z/S.
 {IDirect3DSurface9 *compact=NULL;
  ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE));
  ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLOROP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_COLORARG1,D3DTA_DIFFUSE));
  ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAOP,D3DTOP_SELECTARG1));ok(IDirect3DDevice9_SetTextureStageState(d,0,D3DTSS_ALPHAARG1,D3DTA_DIFFUSE));
  state(d,D3DRS_ZWRITEENABLE,FALSE);state(d,D3DRS_ZENABLE,TRUE);state(d,D3DRS_STENCILENABLE,FALSE);
  state(d,D3DRS_STENCILMASK,0xff);state(d,D3DRS_STENCILWRITEMASK,0xff);state(d,D3DRS_STENCILFUNC,D3DCMP_EQUAL);state(d,D3DRS_STENCILPASS,D3DSTENCILOP_KEEP);state(d,D3DRS_STENCILREF,7);
  ok(IDirect3DDevice9_Clear(d,0,NULL,7,0xff000000u,.75f,3));
  ok(IDirect3DDevice9_CreateRenderTarget(d,8,6,D3DFMT_A8R8G8B8,0,0,FALSE,&compact,NULL));ok(IDirect3DDevice9_SetRenderTarget(d,0,compact));
  ok(IDirect3DDevice9_Clear(d,0,NULL,6,0,.25f,7));ok(IDirect3DDevice9_SetRenderTarget(d,0,back));black(d);
  quad(d,.5f,0xff00ff00u);capture(d,back,D3DFMT_A8R8G8B8);
  black(d);state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILENABLE,TRUE);quad(d,.5f,0xff0000ffu);capture(d,back,D3DFMT_A8R8G8B8);
  record(IDirect3DSurface9_Release(compact));
 }
 // POSITIONT bypasses a bound collapsing VS, while the constant PS still runs.
 {IDirect3DVertexShader9 *vs=NULL,*queried=NULL;IDirect3DPixelShader9 *ps=NULL;IDirect3DVertexDeclaration9 *decl=NULL;
  const DWORD vsCode[]={0xfffe0200,0x0200001f,0x80000000,0x900f0000,0x02000001,0xc00f0000,0xa0e40000,0xffff};
  const DWORD psCode[]={0xffff0200,0x02000001,0x800f0800,0xa0e40000,0xffff};
  const float position[]={0,0,.5f,1},green[]={0,1,0,1};
  const D3DVERTEXELEMENT9 elements[]={{0,0,D3DDECLTYPE_FLOAT4,0,D3DDECLUSAGE_POSITIONT,0},{0,16,D3DDECLTYPE_D3DCOLOR,0,D3DDECLUSAGE_COLOR,0},{0,20,D3DDECLTYPE_FLOAT2,0,D3DDECLUSAGE_TEXCOORD,0},D3DDECL_END()};
  ok(IDirect3DDevice9_CreateVertexShader(d,vsCode,&vs));ok(IDirect3DDevice9_CreatePixelShader(d,psCode,&ps));
  ok(IDirect3DDevice9_SetVertexShaderConstantF(d,0,position,1));ok(IDirect3DDevice9_SetPixelShaderConstantF(d,0,green,1));
  ok(IDirect3DDevice9_SetVertexShader(d,vs));ok(IDirect3DDevice9_SetPixelShader(d,ps));state(d,D3DRS_ZENABLE,FALSE);state(d,D3DRS_STENCILENABLE,FALSE);
  ok(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE|D3DFVF_TEX1));ok(IDirect3DDevice9_GetVertexShader(d,&queried));record(queried==vs);IDirect3DVertexShader9_Release(queried);
  black(d);texture_quad(d);capture(d,back,D3DFMT_A8R8G8B8);
  ok(IDirect3DDevice9_CreateVertexDeclaration(d,elements,&decl));ok(IDirect3DDevice9_SetVertexDeclaration(d,decl));ok(IDirect3DDevice9_GetVertexShader(d,&queried));record(queried==vs);IDirect3DVertexShader9_Release(queried);
  black(d);texture_quad(d);capture(d,back,D3DFMT_A8R8G8B8);
  ok(IDirect3DDevice9_SetVertexDeclaration(d,NULL));ok(IDirect3DDevice9_SetVertexShader(d,NULL));ok(IDirect3DDevice9_SetPixelShader(d,NULL));
  record(IDirect3DVertexDeclaration9_Release(decl));record(IDirect3DVertexShader9_Release(vs));record(IDirect3DPixelShader9_Release(ps));
 }
 record(IDirect3DSurface9_Release(x8));record(IDirect3DTexture9_Release(texture));record(IDirect3DSurface9_Release(back));record(IDirect3DDevice9_Release(d));record(IDirect3D9_Release(factory));CloseHandle(pixel_file);DestroyWindow(w);
 {volatile DWORD guard=0;for(i=0;i<50000;++i)guard=guard*33u+i;record(guard);}record(0xc1ea000du);save();ExitProcess(0);
}
