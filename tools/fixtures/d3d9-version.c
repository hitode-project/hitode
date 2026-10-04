/* SDK-authored factory-only probe: no window, device, rendering or audio. */
#define WIN32_LEAN_AND_MEAN
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
void mainCRTStartup(void) {
    const UINT versions[]={0,30,31,32,33,0x8000001fu,0x80000020u,0xffffffffu};
    DWORD words[41],used=0,written=0;
    for(UINT i=0;i<8;++i) {
        IDirect3D9 *factory=Direct3DCreate9(versions[i]);
        words[used++]=versions[i];words[used++]=factory!=NULL;
        if(factory) {
            words[used++]=IDirect3D9_AddRef(factory);
            words[used++]=IDirect3D9_Release(factory);
            words[used++]=IDirect3D9_Release(factory);
        } else { words[used++]=0;words[used++]=0;words[used++]=0; }
    }
    words[used++]=0xd3d93132u;
    HANDLE file=CreateFileA("d3d9-version.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    if(file==INVALID_HANDLE_VALUE||!WriteFile(file,words,sizeof(words),&written,NULL)||written!=sizeof(words))ExitProcess(1);
    CloseHandle(file);ExitProcess(0);
}
