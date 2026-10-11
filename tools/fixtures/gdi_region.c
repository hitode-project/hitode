/* Native/guest rectangular-region oracle. Build with build-mmio.ps1.
 * No inputs; gdi-region.bin contains API results and synthetic region data. */
#include "win32_min.h"
API HANDLE WINAPI CreateRectRgn(int,int,int,int);
API HANDLE WINAPI CreateRectRgnIndirect(const RECT *);
API BOOL WINAPI SetRectRgn(HANDLE,int,int,int,int);
API int WINAPI GetRgnBox(HANDLE,RECT *);
API BOOL WINAPI PtInRegion(HANDLE,int,int);
API BOOL WINAPI RectInRegion(HANDLE,const RECT *);
API int WINAPI OffsetRgn(HANDLE,int,int);
API DWORD WINAPI GetRegionData(HANDLE,DWORD,void *);
API HANDLE WINAPI ExtCreateRegion(void *,DWORD,const void *);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static DWORD result[256],n;
void *__cdecl memcpy(void *dst,const void *src,unsigned int size){volatile BYTE *d=dst;const BYTE *s=src;while(size--)*d++=*s++;return dst;}
static void record(DWORD v){result[n++]=v;}
static void box(HANDLE h){RECT r={99,99,99,99};record(GetRgnBox(h,&r));record(r.left);record(r.top);record(r.right);record(r.bottom);}
void mainCRTStartup(void){
    RECT rect={20,30,10,5},probe={12,8,15,10};HANDLE h,copy,file;DWORD data[32],written,i,size,mode;
    h=CreateRectRgnIndirect(&rect);record(h!=0);box(h);
    record(PtInRegion(h,10,5));record(PtInRegion(h,20,30));record(RectInRegion(h,&probe));
    record(OffsetRgn(h,-5,2));box(h);record(SetRectRgn(h,0,0,0,100));box(h);record(DeleteObject(h));
    data[0]=32;data[1]=1;data[2]=2;data[3]=32;data[4]=0;data[5]=0;data[6]=30;data[7]=10;
    data[8]=0;data[9]=0;data[10]=10;data[11]=10;data[12]=20;data[13]=0;data[14]=30;data[15]=10;
    h=ExtCreateRegion(0,sizeof(data),data);record(h!=0);box(h);record(PtInRegion(h,15,5));record(PtInRegion(h,25,5));
    size=GetRegionData(h,0,0);record(size);record(GetRegionData(h,sizeof(data),data));for(i=0;i<size/4;i++)record(data[i]);
    copy=ExtCreateRegion(0,size,data);record(copy!=0);box(copy);record(DeleteObject(copy));record(DeleteObject(h));
    for(mode=0;mode<3;mode++){
        data[0]=32;data[1]=1;data[2]=2;data[3]=32;data[4]=0;data[5]=0;data[6]=30;data[7]=30;
        data[8]=0;data[9]=0;data[10]=10;data[11]=mode==2?20:10;
        data[12]=mode==1?0:mode==0?10:20;data[13]=mode==0?0:10;data[14]=mode==1?10:mode==0?20:30;data[15]=mode==2?30:mode==1?20:10;
        h=ExtCreateRegion(0,sizeof(data),data);record(h!=0);box(h);size=GetRegionData(h,0,0);record(size);
        record(GetRegionData(h,sizeof(data),data));for(i=0;i<size/4;i++)record(data[i]);record(DeleteObject(h));
    }
    file=CreateFileA("gdi-region.bin",0x40000000,0,0,2,0,0);
    if(file==0xffffffff||!WriteFile(file,result,n*4,&written,0)||written!=n*4)ExitProcess(250);
    CloseHandle(file);ExitProcess(0);
}
