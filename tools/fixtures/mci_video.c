/* Synthetic MCI video lifecycle oracle. Input: clip.wmv in the working
 * directory. Output: mci-video.bin contains API results and booleans only.
 * Build with build-mmio.ps1; the runtime test generates the clip with FFmpeg. */
#include "win32_min.h"
API DWORD WINAPI mciSendCommandA(UINT,UINT,DWORD,void *);
API DWORD WINAPI mciSendCommandW(UINT,UINT,DWORD,void *);
API BOOL WINAPI PeekMessageA(MSG *,HWND,UINT,UINT,UINT);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static DWORD result[64],n;
static DWORD call(UINT wide,UINT id,UINT message,DWORD flags,DWORD *p){return wide?mciSendCommandW(id,message,flags,p):mciSendCommandA(id,message,flags,p);}
static void record(DWORD v){result[n++]=v;}
void mainCRTStartup(void){
    DWORD wide,id,p[8],written,attempt,found;HANDLE file,window;MSG msg;
    static const WORD type_w[]={'m','p','e','g','v','i','d','e','o',0},file_w[]={'c','l','i','p','.','w','m','v',0};
    for(wide=0;wide<2;wide++){
        p[0]=0;p[1]=0;p[2]=(DWORD)(wide?(const void *)type_w:(const void *)"mpegvideo");p[3]=(DWORD)(wide?(const void *)file_w:(const void *)"clip.wmv");p[4]=0;
        record(call(wide,0,0x803,0x2200,p));id=p[1];record(id!=0);
        if(!id)ExitProcess(10);
        window=CreateWindowExA(0,"STATIC","MCI fixture",0,0,0,32,32,0,0,0,0);
        p[0]=0;p[1]=window;p[2]=0;p[3]=0;record(call(wide,id,0x841,0x50000,p));
        p[0]=0;p[1]=0;p[2]=0;record(call(wide,id,0x806,6,p));
        p[0]=0;p[1]=0;p[2]=4;p[3]=0;record(call(wide,id,0x814,0x100,p));record(p[1]);
        p[0]=0;p[1]=0;p[2]=1;p[3]=0;record(call(wide,id,0x814,0x100,p));record(p[1]>0);
        p[0]=window;p[1]=0;p[2]=0;record(call(wide,id,0x806,5,p));
        record(call(wide,id,0x806,5,p));found=0;
        for(attempt=0;attempt<100;attempt++){if(PeekMessageA(&msg,window,0x3b9,0x3b9,1)){found=1;break;}Sleep(5);}
        record(found);record(found?msg.wparam:0);record(found&&msg.lparam==id);
        record(call(wide,id,0x806,1,p));found=0;
        for(attempt=0;attempt<100;attempt++){if(PeekMessageA(&msg,window,0x3b9,0x3b9,1)){found=1;break;}Sleep(5);}
        record(found);record(found?msg.wparam:0);record(found&&msg.lparam==id);
        p[0]=0;record(call(wide,id,0x808,2,p));found=0;
        for(attempt=0;attempt<100;attempt++){if(PeekMessageA(&msg,window,0x3b9,0x3b9,1)){found=1;break;}Sleep(5);}
        record(found);record(found?msg.wparam:0);record(found&&msg.lparam==id);
        p[0]=window;p[1]=0;p[2]=0;record(call(wide,id,0x806,5,p));
        p[0]=0;record(call(wide,id,0x804,0,p));found=0;
        for(attempt=0;attempt<100;attempt++){if(PeekMessageA(&msg,window,0x3b9,0x3b9,1)){found=1;break;}Sleep(5);}
        record(found);record(found?msg.wparam:0);record(found&&msg.lparam==id);
        p[0]=0;p[1]=0;p[2]=4;p[3]=0;record(call(wide,id,0x814,0x100,p)!=0);
        record(DestroyWindow(window)!=0);
    }
    file=CreateFileA("mci-video.bin",0x40000000,0,0,2,0,0);
    if(file==0xffffffff||!WriteFile(file,result,n*4,&written,0)||written!=n*4)ExitProcess(250);
    CloseHandle(file);ExitProcess(0);
}
