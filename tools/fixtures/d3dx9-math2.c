/* Authored x86 D3DX scalar rotations and Vec2 oracle. No input; output is scalar.
 * Build: build-d3d9.ps1 -Name d3dx9-math2. Writes d3dx9-math2.bin below the CWD. */
#include <windows.h>
int _fltused=0;
typedef struct {float v[16];} MATRIX;
typedef struct {float v[2];} VECTOR2;
typedef struct {float v[4];} VECTOR4;
typedef MATRIX *(WINAPI *ROTATE)(MATRIX*,float);
typedef void *(WINAPI *TRANSFORM)(void*,const VECTOR2*,const MATRIX*);
static DWORD output[4096],used;
static void record(DWORD value){output[used++]=value;}
static void result(UINT count,void *pointer,void *returned){record(count);record(pointer==returned);for(UINT i=0;i<count;i++)record(((DWORD*)pointer)[i]);}
void mainCRTStartup(void){
 const char *modules[]={"d3dx9_29.dll","d3dx9_43.dll"};const char *rotations[]={"D3DXMatrixRotationX","D3DXMatrixRotationZ"};const char *vectors[]={"D3DXVec2Transform","D3DXVec2TransformCoord","D3DXVec2TransformNormal"};
 const float angles[]={0,-0.5f,0.25f,1.5707963267948966f,1000};
 for(UINT module=0;module<2;module++){HMODULE dll=LoadLibraryA(modules[module]);if(!dll)ExitProcess(1);
  for(UINT axis=0;axis<2;axis++){ROTATE rotate=(ROTATE)GetProcAddress(dll,rotations[axis]);if(!rotate)ExitProcess(2);for(UINT angle=0;angle<5;angle++){MATRIX out;result(16,&out,rotate(&out,angles[angle]));}}
  for(UINT operation=0;operation<3;operation++){TRANSFORM transform=(TRANSFORM)GetProcAddress(dll,vectors[operation]);if(!transform)ExitProcess(3);for(UINT test=0;test<4;test++){
   MATRIX matrix={{2,3,4,.25f,5,6,7,.5f,8,9,10,11,12,13,14,2}};VECTOR4 out={{2,-3,99,99}};VECTOR2 *input=(VECTOR2*)&out;
   if(test==1)matrix.v[15]=1;else if(test==2){matrix.v[3]=matrix.v[7]=matrix.v[15]=0;}else if(test==3){for(UINT i=8;i<12;i++)((DWORD*)matrix.v)[i]=0x7fc12345;for(UINT i=12;i<16;i++)((DWORD*)matrix.v)[i]=0x7fc12345;}
   result(operation==0?4:2,&out,transform(&out,input,&matrix));
  }}FreeLibrary(dll);
 }
 HANDLE file=CreateFileA("d3dx9-math2.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;if(file==INVALID_HANDLE_VALUE||!WriteFile(file,output,used*4,&written,0)||written!=used*4)ExitProcess(4);CloseHandle(file);ExitProcess(0);
}
