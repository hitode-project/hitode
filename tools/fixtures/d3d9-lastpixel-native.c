/*
 * Authored x86 LASTPIXEL oracle using a hidden offscreen window, without Present or game data.
 * Inputs: line geometries, state values, diffuse colors, blending, and a D16 depth case.
 * Build from the repository root: ./tools/fixtures/build-d3d9-lastpixel-native.ps1
 * Add -Run to execute HAL and REF cases; LLVM and the Windows SDK are required.
 * Default output: target/fixtures/d3d9-lastpixel-native.
 * environment.json records toolchain details and hashes; native.jsonl records HRESULTs,
 * state readback, device labels, pixel coordinates, and ARGB values after GetRenderTargetData.
 * HAL and REF results are separate observations, not interchangeable expected output.
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int c,unsigned n){BYTE*q=p;while(n--)*q++=(BYTE)c;return p;}
typedef struct {float x,y,z,rhw;DWORD color;} Vertex;
typedef struct {const char*name;D3DPRIMITIVETYPE type;UINT n;int q[8];DWORD colors[4];UINT blend,depth;} Case;
static const Case cases[]={
 {"horizontal",D3DPT_LINELIST,2,{8,12,28,12},{0xffff0000,0xffff0000}},
 {"vertical",D3DPT_LINELIST,2,{12,8,12,28},{0xffff0000,0xffff0000}},
 {"horizontal_reverse",D3DPT_LINELIST,2,{28,12,8,12},{0xffff0000,0xffff0000}},
 {"diagonal",D3DPT_LINELIST,2,{8,8,28,28},{0xffff0000,0xffff0000}},
 {"diagonal_reverse",D3DPT_LINELIST,2,{28,28,8,8},{0xffff0000,0xffff0000}},
 {"shallow",D3DPT_LINELIST,2,{8,8,32,20},{0xffff0000,0xffff0000}},
 {"clip_start",D3DPT_LINELIST,2,{-8,20,28,20},{0xffff0000,0xffff0000}},
 {"clip_end",D3DPT_LINELIST,2,{8,24,80,24},{0xffff0000,0xffff0000}},
 {"subpixel_quarter",D3DPT_LINELIST,2,{9,13,29,13},{0xffff0000,0xffff0000}},
 {"subpixel_threequarter",D3DPT_LINELIST,2,{11,15,31,15},{0xffff0000,0xffff0000}},
 {"strip_turn_alpha",D3DPT_LINESTRIP,3,{8,12,28,12,28,32},{0x80ff0000,0x80ff0000,0x80ff0000},1},
 {"strip_collinear_alpha",D3DPT_LINESTRIP,3,{8,12,20,12,32,12},{0x80ff0000,0x80ff0000,0x80ff0000},1},
 {"color_alpha_gradient",D3DPT_LINELIST,2,{8,12,28,12},{0x40ff0000,0xc00000ff},1},
 {"depth_gradient",D3DPT_LINELIST,2,{8,12,28,12},{0xffff0000,0xffff0000},0,1},
 {"list_joint_alpha",D3DPT_LINELIST,4,{8,12,28,12,28,12,28,32},{0x80ff0000,0x80ff0000,0x80ff0000,0x80ff0000},1},
 {"strip_tail_degenerate_color",D3DPT_LINESTRIP,3,{8,12,28,12,28,12},{0x80ff0000,0x80ff0000,0xc00000ff},1},
 {"same_point",D3DPT_LINELIST,2,{12,12,12,12},{0xffff0000,0xffff0000}},
};
static HANDLE output;
static void emit(const char*s){DWORD w;WriteFile(output,s,lstrlenA(s),&w,0);}
static int failed(HRESULT hr,const char*at){char b[160];if(hr==D3D_OK)return 0;wsprintfA(b,"{\"op\":\"failure\",\"at\":\"%s\",\"hr\":\"%08x\"}\n",at,(UINT)hr);emit(b);return 1;}
#define OK(x) do{if(failed((x),#x))ExitProcess(4);}while(0)
static UINT cases_done;
static void run_cases(IDirect3DDevice9*d,IDirect3DSurface9*back,IDirect3DSurface9*read,const char*device_name){
 char b[16384];UINT i,last,used,x,y,j;
 OK(IDirect3DDevice9_SetFVF(d,D3DFVF_XYZRHW|D3DFVF_DIFFUSE));OK(IDirect3DDevice9_SetTexture(d,0,0));
 OK(IDirect3DDevice9_SetRenderState(d,D3DRS_LIGHTING,FALSE));OK(IDirect3DDevice9_SetRenderState(d,D3DRS_CULLMODE,D3DCULL_NONE));
 OK(IDirect3DDevice9_SetRenderState(d,D3DRS_ANTIALIASEDLINEENABLE,FALSE));OK(IDirect3DDevice9_SetRenderState(d,D3DRS_MULTISAMPLEANTIALIAS,FALSE));
 OK(IDirect3DDevice9_SetRenderState(d,D3DRS_ALPHATESTENABLE,FALSE));OK(IDirect3DDevice9_SetRenderState(d,D3DRS_STENCILENABLE,FALSE));
 OK(IDirect3DDevice9_SetRenderState(d,D3DRS_SRCBLEND,D3DBLEND_SRCALPHA));OK(IDirect3DDevice9_SetRenderState(d,D3DRS_DESTBLEND,D3DBLEND_INVSRCALPHA));OK(IDirect3DDevice9_SetRenderState(d,D3DRS_BLENDOP,D3DBLENDOP_ADD));
 for(i=0;i<sizeof(cases)/sizeof(cases[0]);i++)for(last=0;last<2;last++){
  const Case*c=&cases[i];Vertex v[4];D3DLOCKED_RECT lock;UINT count=0;DWORD actual_last;
  for(j=0;j<c->n;j++){v[j].x=(float)c->q[2*j]*.25f;v[j].y=(float)c->q[2*j+1]*.25f;v[j].z=c->depth?(j?.8f:.2f):.5f;v[j].rhw=1;v[j].color=c->colors[j];}
  OK(IDirect3DDevice9_SetRenderState(d,D3DRS_LASTPIXEL,last));OK(IDirect3DDevice9_SetRenderState(d,D3DRS_ALPHABLENDENABLE,c->blend));OK(IDirect3DDevice9_SetRenderState(d,D3DRS_ZENABLE,c->depth));OK(IDirect3DDevice9_SetRenderState(d,D3DRS_ZWRITEENABLE,FALSE));OK(IDirect3DDevice9_SetRenderState(d,D3DRS_ZFUNC,D3DCMP_LESS));
  OK(IDirect3DDevice9_GetRenderState(d,D3DRS_LASTPIXEL,&actual_last));
  OK(IDirect3DDevice9_Clear(d,0,0,D3DCLEAR_TARGET|D3DCLEAR_ZBUFFER,0xff000000,.5f,0));OK(IDirect3DDevice9_BeginScene(d));
  OK(IDirect3DDevice9_DrawPrimitiveUP(d,c->type,c->type==D3DPT_LINESTRIP?c->n-1:c->n/2,v,sizeof(v[0])));OK(IDirect3DDevice9_EndScene(d));
  OK(IDirect3DDevice9_GetRenderTargetData(d,back,read));OK(IDirect3DSurface9_LockRect(read,&lock,0,D3DLOCK_READONLY));
  used=wsprintfA(b,"{\"op\":\"case\",\"device\":\"%s\",\"name\":\"%s\",\"lastpixel\":%u,\"actual_lastpixel\":%u,\"type\":%u,\"alpha_blend\":%u,\"z_less_half\":%u,\"vertices\":[",device_name,c->name,last,actual_last,c->type,c->blend,c->depth);
  for(j=0;j<c->n;j++)used+=wsprintfA(b+used,"%s{\"x_q\":%d,\"y_q\":%d,\"z_bits\":\"%08x\",\"argb\":\"%08x\"}",j?",":"",c->q[2*j],c->q[2*j+1],*(DWORD*)&v[j].z,v[j].color);
  used+=wsprintfA(b+used,"],\"pixels\":[");
  for(y=0;y<16;y++)for(x=0;x<16;x++){DWORD color=((DWORD*)((BYTE*)lock.pBits+y*lock.Pitch))[x];if(color!=0xff000000){used+=wsprintfA(b+used,"%s[%u,%u,\"%08x\"]",count?",":"",x,y,color);count++;}}
  wsprintfA(b+used,"],\"count\":%u,\"all_hresult_s_ok\":true}\n",count);emit(b);cases_done++;OK(IDirect3DSurface9_UnlockRect(read));
 }
}
void mainCRTStartup(void){
 HWND w;IDirect3D9*f;IDirect3DDevice9*d=0;IDirect3DSurface9*back=0,*read=0;
 D3DPRESENT_PARAMETERS p={0};D3DADAPTER_IDENTIFIER9 id={0};D3DCAPS9 caps={0};
 UINT i,last;DWORD default_last;char b[16384],hex[1100];UINT used,x,y,j;
 output=GetStdHandle(STD_OUTPUT_HANDLE);
 w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","authored D3D9 line oracle",WS_POPUP,-32000,-32000,32,32,0,0,GetModuleHandleA(0),0);
 f=Direct3DCreate9(D3D_SDK_VERSION);if(!w||!f){emit("{\"op\":\"failure\",\"at\":\"window/factory\"}\n");ExitProcess(2);}
 OK(IDirect3D9_GetAdapterIdentifier(f,0,0,&id));
 p.BackBufferWidth=16;p.BackBufferHeight=16;p.BackBufferFormat=D3DFMT_A8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=1;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;p.EnableAutoDepthStencil=1;p.AutoDepthStencilFormat=D3DFMT_D16;
 OK(IDirect3D9_CreateDevice(f,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&d));
 OK(IDirect3DDevice9_GetDeviceCaps(d,&caps));OK(IDirect3DDevice9_GetRenderState(d,D3DRS_LASTPIXEL,&default_last));
 used=0;for(i=0;id.Description[i]&&i<sizeof(id.Description);i++)used+=wsprintfA(hex+used,"%02x",(BYTE)id.Description[i]);
 wsprintfA(b,"{\"op\":\"environment\",\"sdk\":32,\"device\":\"HAL\",\"backbuffer\":[16,16],\"format\":21,\"depth_format\":80,\"fvf\":68,\"stride\":20,\"multisample\":0,\"antialiased_line\":0,\"default_lastpixel\":%u,\"vendor\":%u,\"device_id\":%u,\"driver_version_hi\":%u,\"driver_version_lo\":%u,\"line_caps\":%u,\"description_acp_hex\":\"%s\"}\n",default_last,id.VendorId,id.DeviceId,id.DriverVersion.HighPart,id.DriverVersion.LowPart,caps.LineCaps,hex);emit(b);
 OK(IDirect3DDevice9_GetBackBuffer(d,0,0,D3DBACKBUFFER_TYPE_MONO,&back));OK(IDirect3DDevice9_CreateOffscreenPlainSurface(d,16,16,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&read,0));
 run_cases(d,back,read,"HAL");
 {
  IDirect3DDevice9*ref=0;HRESULT hr=IDirect3D9_CreateDevice(f,0,D3DDEVTYPE_REF,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&ref);
  wsprintfA(b,"{\"op\":\"reference_availability\",\"create_hr\":\"%08x\"}\n",(UINT)hr);emit(b);
  if(SUCCEEDED(hr)){
   IDirect3DSurface9*rb=0,*rr=0;
   OK(IDirect3DDevice9_GetBackBuffer(ref,0,0,D3DBACKBUFFER_TYPE_MONO,&rb));OK(IDirect3DDevice9_CreateOffscreenPlainSurface(ref,16,16,D3DFMT_A8R8G8B8,D3DPOOL_SYSTEMMEM,&rr,0));
   run_cases(ref,rb,rr,"REF");
   IDirect3DSurface9_Release(rr);IDirect3DSurface9_Release(rb);IDirect3DDevice9_Release(ref);
  }
 }
 IDirect3DSurface9_Release(read);IDirect3DSurface9_Release(back);IDirect3DDevice9_Release(d);IDirect3D9_Release(f);DestroyWindow(w);wsprintfA(b,"{\"op\":\"complete\",\"case_count\":%u,\"result\":1}\n",cases_done);emit(b);ExitProcess(0);
}
