/* CPU-only native shader semantic-query ABI oracle, own fxc bytecode. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>
#include "d3d9-shaders-generated.h"
typedef struct {UINT Usage,UsageIndex;} Semantic;
typedef HRESULT(WINAPI *OutputSemantics)(const DWORD*,Semantic*,UINT*);
static DWORD result[128];static UINT used;
static void record(DWORD n){if(used>=128)ExitProcess(99);result[used++]=n;}
void mainCRTStartup(void){
 HMODULE lib=LoadLibraryA("d3dx9_43.dll");OutputSemantics query=(OutputSemantics)GetProcAddress(lib,"D3DXGetShaderOutputSemantics");
 const BYTE*programs[5]={vs20,ps20_color,ps20_texture,vs30,ps30_flow};Semantic semantics[32];UINT i,j,n;DWORD written;HANDLE file;
 if(!query)ExitProcess(1);
 for(i=0;i<5;++i){n=0;record(query((const DWORD*)programs[i],NULL,&n));record(n);for(j=0;j<32;++j){semantics[j].Usage=0xccccccccu;semantics[j].UsageIndex=0xccccccccu;}n=1;record(query((const DWORD*)programs[i],semantics,&n));record(n);for(j=0;j<n&&j<32;++j){record(semantics[j].Usage);record(semantics[j].UsageIndex);}}
 record(query(NULL,NULL,&n));{volatile DWORD guard=0;for(i=0;i<50000;++i)guard=guard*33u+i;record(guard);}record(0x5ade5e00u);file=CreateFileA("d3d9-shaders-semantics.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(file==INVALID_HANDLE_VALUE)ExitProcess(90);if(!WriteFile(file,result,used*4,&written,NULL)||written!=used*4)ExitProcess(91);CloseHandle(file);FreeLibrary(lib);ExitProcess(0);
}
