/* Authored D3DX texture creation/requirements oracle, no external input.
 * Only scalar results and deterministic lock data are written below the CWD. */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
typedef HRESULT (WINAPI *CHECK)(IDirect3DDevice9*,UINT*,UINT*,UINT*,DWORD,D3DFORMAT*,D3DPOOL);
typedef HRESULT (WINAPI *CREATE)(IDirect3DDevice9*,UINT,UINT,UINT,DWORD,D3DFORMAT,D3DPOOL,IDirect3DTexture9**);
void *memset(void *p,int v,size_t n){volatile unsigned char *q=p;while(n--)*q++=(unsigned char)v;return p;}
static DWORD values[4096],used;
static void record(DWORD value){values[used++]=value;}
static void require(HRESULT h){if(FAILED(h))ExitProcess(100+used);}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","authored texture oracle",WS_OVERLAPPED,-32000,-32000,64,64,0,0,GetModuleHandleA(0),0);
 IDirect3D9 *factory=Direct3DCreate9(32);IDirect3DDevice9 *device=0;D3DPRESENT_PARAMETERS p={0};HMODULE dll=LoadLibraryA("d3dx9_43.dll");CHECK check=(CHECK)GetProcAddress(dll,"D3DXCheckTextureRequirements");CREATE create=(CREATE)GetProcAddress(dll,"D3DXCreateTexture");
 if(!w||!factory||!dll||!check||!create)ExitProcess(1);
 p.BackBufferWidth=16;p.BackBufferHeight=12;p.BackBufferFormat=D3DFMT_X8R8G8B8;p.BackBufferCount=1;p.SwapEffect=D3DSWAPEFFECT_DISCARD;p.hDeviceWindow=w;p.Windowed=1;p.PresentationInterval=D3DPRESENT_INTERVAL_IMMEDIATE;
 require(IDirect3D9_CreateDevice(factory,0,D3DDEVTYPE_HAL,w,D3DCREATE_SOFTWARE_VERTEXPROCESSING|D3DCREATE_FPU_PRESERVE,&p,&device));
 const UINT cases[][7]={{0,0,0,0,21,1,1},{~0u,~0u,~0u,0,21,1,0},{~0u,12,99,0,22,1,1},{19,7,99,0,26,1,1},{6,3,1,0,0,1,1},{6,3,1,0,28,1,1},{6,3,1,1,21,0,1},{6,3,1,0x200,21,0,1},{6,3,1,1,21,1,0},{6,3,1,0x200,21,1,0},{6,3,0,1,21,0,1},{6,3,99,0,22,2,1}};
 for(UINT i=0;i<sizeof(cases)/sizeof(cases[0]);i++){
  UINT width=cases[i][0],height=cases[i][1],mips=cases[i][2];D3DFORMAT format=cases[i][4];HRESULT result=check(device,&width,&height,&mips,cases[i][3],&format,cases[i][5]);record(result);record(width);record(height);record(mips);record(format);
  if(cases[i][6]){IDirect3DTexture9 *texture=0;result=create(device,cases[i][0],cases[i][1],cases[i][2],cases[i][3],cases[i][4],cases[i][5],&texture);record(result);record(texture!=0);if(SUCCEEDED(result)&&texture){record(IDirect3DTexture9_GetLevelCount(texture));for(UINT level=0;level<IDirect3DTexture9_GetLevelCount(texture);level++){D3DSURFACE_DESC desc;require(IDirect3DTexture9_GetLevelDesc(texture,level,&desc));record(desc.Width);record(desc.Height);record(desc.Format);record(desc.Usage);record(desc.Pool);if(desc.Usage==0 && desc.Pool!=0){D3DLOCKED_RECT lock;record(IDirect3DTexture9_LockRect(texture,level,&lock,0,0));*((BYTE*)lock.pBits)=(BYTE)(0x40+level);record(IDirect3DTexture9_UnlockRect(texture,level));record(IDirect3DTexture9_LockRect(texture,level,&lock,0,D3DLOCK_READONLY));record(*((BYTE*)lock.pBits)==(BYTE)(0x40+level));record(IDirect3DTexture9_UnlockRect(texture,level));}}record(IDirect3DTexture9_Release(texture));}}
 }
 record(check(device,0,0,0,0,0,D3DPOOL_MANAGED));record(check(0,0,0,0,0,0,D3DPOOL_MANAGED));
 for(UINT mask=0;mask<16;mask++){UINT width=7,height=3,mips=99;D3DFORMAT format=D3DFMT_UNKNOWN;record(check(device,mask&1?&width:0,mask&2?&height:0,mask&4?&mips:0,0,mask&8?&format:0,D3DPOOL_MANAGED));record(width);record(height);record(mips);record(format);}
 for(UINT i=3;i<5;i++){UINT width=7,height=3,mips=1;D3DFORMAT format=21;record(check(device,&width,&height,&mips,i==1?0x4000:i==2?2:0,&format,i==3?4:i==4?-1:1));record(width);record(height);record(mips);record(format);} IDirect3DTexture9 *invalid=(IDirect3DTexture9*)0x1234;record(create(0,1,1,1,0,21,1,&invalid));record(invalid==0);record(create(device,1,1,1,0,21,1,0)); HMODULE older=LoadLibraryA("d3dx9_29.dll");CHECK oldcheck=(CHECK)GetProcAddress(older,"D3DXCheckTextureRequirements");CREATE oldcreate=(CREATE)GetProcAddress(older,"D3DXCreateTexture");if(!older||!oldcheck||!oldcreate)ExitProcess(3);UINT aw=0,ah=0,am=0;D3DFORMAT af=21;record(oldcheck(device,&aw,&ah,&am,0,&af,1));record(aw);record(ah);record(am);record(af);IDirect3DTexture9 *oldtexture=0;record(oldcreate(device,3,2,0,0,21,1,&oldtexture));record(oldtexture!=0);if(oldtexture){record(IDirect3DTexture9_GetLevelCount(oldtexture));record(IDirect3DTexture9_Release(oldtexture));}FreeLibrary(older); IDirect3DDevice9_Release(device);IDirect3D9_Release(factory);FreeLibrary(dll);DestroyWindow(w);
 HANDLE file=CreateFileA("d3dx9-texture-create.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,used*4,&written,0)||written!=used*4)ExitProcess(2);CloseHandle(file);ExitProcess(0);
}
