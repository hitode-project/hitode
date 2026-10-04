/* Authored no-window, no-CRT native observation of indexed DIB table bounds. */
#include "win32_min.h"
API UINT WINAPI GetDIBColorTable(HDC,UINT,UINT,void *);
API UINT WINAPI SetDIBColorTable(HDC,UINT,UINT,const void *);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static struct { BITMAPINFOHEADER header; DWORD colors[256]; } info;
static DWORD input[256],output[260],records[256],count;
static const DWORD tail[4]={0xe1448855,0xf2669911,0xa377aa22,0xb488bb33};
static void record(DWORD tag,DWORD value) { records[count++]=tag;records[count++]=value; }
static void reset_output(void) { DWORD i;for(i=0;i<260;++i)output[i]=0xdec0de00+i; }
static void observe(DWORD used,DWORD tag) {
    DWORD i,capacity=used?used:256;void *bits;HANDLE previous;
    HDC dc=CreateCompatibleDC(0);HBITMAP bitmap;
    info.header.size=40;info.header.width=4;info.header.height=-2;
    info.header.planes=1;info.header.bpp=8;info.header.used=used;
    for(i=0;i<256;++i) { info.colors[i]=0xa5123456+i;input[i]=0xb2234567+i; }
    input[0]=0xa1336699;input[1]=0xb2102030;input[2]=0xc3557799;
    bitmap=CreateDIBSection(dc,(const BITMAPINFO *)&info,0,&bits,0,0);
    record(tag,used);record(tag+1,dc!=0&&bitmap!=0);
    if(!dc||!bitmap) ExitProcess(10);
    previous=SelectObject(dc,bitmap);
    reset_output();record(tag+2,GetDIBColorTable(dc,0,256,output));
    record(tag+3,output[0]);
    record(tag+4,SetDIBColorTable(dc,0,256,input));
    reset_output();record(tag+5,GetDIBColorTable(dc,0,256,output));
    record(tag+6,output[0]);record(tag+7,output[1]);record(tag+8,output[2]);
    record(tag+9,output[capacity]);
    record(tag+10,SetDIBColorTable(dc,capacity-1,4,tail));
    reset_output();record(tag+11,GetDIBColorTable(dc,capacity-1,4,output));
    record(tag+12,output[0]);record(tag+13,output[1]);
    record(tag+14,SetDIBColorTable(dc,capacity,4,tail));
    reset_output();record(tag+15,GetDIBColorTable(dc,capacity,4,output));
    record(tag+16,output[0]);
    SelectObject(dc,previous);DeleteObject(bitmap);DeleteDC(dc);
}
void mainCRTStartup(void) {
    HANDLE file;DWORD written;
    observe(0,1000);observe(2,2000);observe(3,3000);
    record(9999,0x44494243);
    file=CreateFileA("dib-color-table.bin",0x40000000,0,0,2,0,0);
    if(file==0xffffffffu||!WriteFile(file,records,count*4,&written,0)||written!=count*4)ExitProcess(2);
    CloseHandle(file);ExitProcess(0);
}
