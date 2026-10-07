/* Authored BMP/ID3DXBuffer contract probe. Input: installed D3D9/D3DX9.
 * Output: boolean DWORD assertions only in d3dx9-surface-save.bin. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
int _fltused;
void *memset(void *p,int c,unsigned n){unsigned char *q=p;while(n--)*q++=(unsigned char)c;return p;}
typedef struct Buffer Buffer;
typedef struct { HRESULT (WINAPI *query)(Buffer*,REFIID,void**); ULONG (WINAPI *add)(Buffer*); ULONG (WINAPI *release)(Buffer*); void* (WINAPI *data)(Buffer*); DWORD (WINAPI *size)(Buffer*); } BufferTable;
struct Buffer {BufferTable *v;};
typedef HRESULT (WINAPI *Load)(IDirect3DDevice9*,const void*,UINT,UINT,UINT,UINT,DWORD,D3DFORMAT,D3DPOOL,DWORD,DWORD,D3DCOLOR,void*,void*,IDirect3DTexture9**);
typedef HRESULT (WINAPI *SaveFile)(LPCWSTR,UINT,IDirect3DSurface9*,const PALETTEENTRY*,const RECT*);
typedef HRESULT (WINAPI *Save)(Buffer**,UINT,IDirect3DSurface9*,const PALETTEENTRY*,const RECT*);
static DWORD checks[512],count;
static BYTE filebytes[1024];static DWORD filelength;
static void* WINAPI filedata(Buffer *b){(void)b;return filebytes;}
static DWORD WINAPI filesize(Buffer *b){(void)b;return filelength;}
static void check(BOOL value){checks[count++]=!!value;}
static void finish(void){DWORD written;HANDLE f=CreateFileA("d3dx9-surface-save.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);if(f==INVALID_HANDLE_VALUE)ExitProcess(90);WriteFile(f,checks,count*4,&written,0);CloseHandle(f);ExitProcess(written==count*4?0:91);}
static void required(HRESULT hr){check(SUCCEEDED(hr));if(FAILED(hr))finish();}
static const GUID iid={0x8ba5fb08,0x5195,0x40e2,{0xac,0x58,0x0d,0x98,0x9c,0x3a,1,2}};
static const GUID unknown={0,0,0,{0xc0,0,0,0,0,0,0,0x46}};
static void texture_roundtrip(IDirect3DDevice9 *device,Buffer *buffer){
 Load load=(Load)GetProcAddress(GetModuleHandleA("d3dx9_43.dll"),"D3DXCreateTextureFromFileInMemoryEx");
 const DWORD green[3][4]={{30,90,150,0},{30,30,90,150},{45,68,113,135}};
 UINT filter,x,y;check(load!=0);if(!load)return;
 for(filter=1;filter<=3;filter++){
  IDirect3DTexture9 *texture=0;D3DSURFACE_DESC desc;D3DLOCKED_RECT lock;DWORD info[7]={0};
  required(load(device,buffer->v->data(buffer),buffer->v->size(buffer),0,0,1,0,21,1,filter,1,0,info,0,&texture));
  check(info[0]==4&&info[1]==3);
  required(IDirect3DTexture9_GetLevelDesc(texture,0,&desc));check(desc.Width==4&&desc.Height==4);
  required(IDirect3DTexture9_LockRect(texture,0,&lock,0,16));
  for(y=0;y<4;y++)for(x=0;x<4;x++){
   DWORD expected=(filter==1&&y==3)?0:(0x7f1f0000|(green[filter-1][y]<<8)|(20+x*40));
   check(((DWORD*)((BYTE*)lock.pBits+y*lock.Pitch))[x]==expected);
  }
  required(IDirect3DTexture9_UnlockRect(texture,0));check(IDirect3DTexture9_Release(texture)==0);
 }
}
static void image(Buffer *b,UINT width,UINT height,UINT left,UINT top){
 BYTE *bytes=b->v->data(b);DWORD size=b->v->size(b),offset,stride,y,x,bits;LONG h;
 check(bytes!=0);check(size>=54);if(!bytes||size<54)return;
 offset=*(DWORD*)(bytes+10);bits=*(WORD*)(bytes+28);h=*(LONG*)(bytes+22);
 check(bytes[0]=='B'&&bytes[1]=='M');check(*(DWORD*)(bytes+2)==size);
 check(*(DWORD*)(bytes+18)==width);check(h==(LONG)height||h==-(LONG)height);
 check(bits==32);if(bits!=24&&bits!=32)return;
 stride=((width*bits+31)/32)*4;check(offset+stride*height<=size);if(offset+stride*height>size)return;
 for(y=0;y<height;y++)for(x=0;x<width;x++){
  BYTE *p=bytes+offset+(h>0?height-1-y:y)*stride+x*(bits/8);
  check(bits==32 && p[3]==127);
  check(left==99 ? (p[0]==1&&p[1]==11&&p[2]==31) : (p[0]==20+(left+x)*40&&p[1]==30+(top+y)*60&&p[2]==31));
 }
}
void mainCRTStartup(void){
 HWND w=CreateWindowExA(WS_EX_TOOLWINDOW|WS_EX_NOACTIVATE,"STATIC","BMP oracle",WS_OVERLAPPED,-32000,-32000,32,32,0,0,GetModuleHandleA(0),0);
 IDirect3D9 *f=Direct3DCreate9(32);IDirect3DDevice9 *d=0;IDirect3DSurface9 *s=0,*target=0,*back=0;D3DPRESENT_PARAMETERS pp={0};D3DLOCKED_RECT lock;UINT x,y;Buffer *b=0,*q=0;RECT crop={1,1,3,3},bad={-1,0,2,2};
 Save save=(Save)GetProcAddress(LoadLibraryA("d3dx9_43.dll"),"D3DXSaveSurfaceToFileInMemory");
 check(w&&f&&save);if(!w||!f||!save)finish();
 pp.BackBufferWidth=8;pp.BackBufferHeight=6;pp.BackBufferFormat=21;pp.BackBufferCount=1;pp.SwapEffect=1;pp.hDeviceWindow=w;pp.Windowed=1;pp.PresentationInterval=0x80000000;
 required(IDirect3D9_CreateDevice(f,0,1,w,0x22,&pp,&d));
 required(IDirect3DDevice9_CreateOffscreenPlainSurface(d,4,3,21,D3DPOOL_SYSTEMMEM,&s,0));
 required(IDirect3DSurface9_LockRect(s,&lock,0,0));
 for(y=0;y<3;y++)for(x=0;x<4;x++)((DWORD*)((BYTE*)lock.pBits+y*lock.Pitch))[x]=0x7f1f0000|((30+y*60)<<8)|(20+x*40);
 required(IDirect3DSurface9_UnlockRect(s));
 required(save(&b,0,s,0,0));image(b,4,3,0,0);texture_roundtrip(d,b);
 check(b->v->add(b)==2);check(b->v->release(b)==1);
 required(b->v->query(b,&iid,(void**)&q));check(q==b);check(q->v->release(q)==1);
 required(b->v->query(b,&unknown,(void**)&q));check(q==b);check(q->v->release(q)==1);
 check(b->v->release(b)==0);b=0;
 required(save(&b,0,s,0,&crop));image(b,2,2,1,1);check(b->v->release(b)==0);b=0;
 { SaveFile savefile=(SaveFile)GetProcAddress(GetModuleHandleA("d3dx9_43.dll"),"D3DXSaveSurfaceToFileW");
   static const WCHAR path[]=L"thumb-\x5b58\x6863.bmp";
   BufferTable table={0,0,0,filedata,filesize};Buffer filebuffer={&table};HANDLE file;
   check(savefile!=0);if(!savefile)finish();required(savefile(path,0,s,0,0));
   file=CreateFileW(path,GENERIC_READ,1,0,OPEN_EXISTING,0,0);check(file!=INVALID_HANDLE_VALUE);if(file==INVALID_HANDLE_VALUE)finish();
   check(ReadFile(file,filebytes,sizeof(filebytes),&filelength,0));CloseHandle(file);image(&filebuffer,4,3,0,0);
   /* Exercise failure through ordinary exclusive sharing. Parent creation is
      an independent VFS policy, not part of this image-export fixture. */
   file=CreateFileW(path,GENERIC_READ,0,0,OPEN_EXISTING,0,0);check(file!=INVALID_HANDLE_VALUE);
   check(FAILED(savefile(path,0,s,0,0)));CloseHandle(file);
 }
 check(FAILED(save(&b,0,s,0,&bad)));
 required(IDirect3DDevice9_CreateRenderTarget(d,4,3,21,0,0,FALSE,&target,0));
 required(IDirect3DDevice9_GetRenderTarget(d,0,&back));
 required(IDirect3DDevice9_SetRenderTarget(d,0,target));
 { D3DVIEWPORT9 viewport={0,0,4,3,0,1};required(IDirect3DDevice9_SetViewport(d,&viewport)); }
 required(IDirect3DDevice9_Clear(d,0,0,1,0x7f1f0b01,1,0));
 required(IDirect3DDevice9_SetRenderTarget(d,0,back));
 IDirect3DSurface9_Release(back);
 required(save(&b,0,target,0,0));image(b,4,3,99,0);check(b->v->release(b)==0);b=0;
 check(IDirect3DSurface9_Release(target)==0);
 check(IDirect3DSurface9_Release(s)==0);check(IDirect3DDevice9_Release(d)==0);check(IDirect3D9_Release(f)==0);
 finish();
}
