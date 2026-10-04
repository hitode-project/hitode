/* SDK-authored memory-only USER32 probe; no clipboard content or host setting writes. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
void *memset(void *p,int v,unsigned n){BYTE *b=p;while(n--)*b++=(BYTE)v;return p;}
static DWORD words[256],used;
static void record(DWORD value){if(used==256)ExitProcess(90);words[used++]=value;}
static void save(void){DWORD n;HANDLE f=CreateFileA("user32-blockers.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);if(f==INVALID_HANDLE_VALUE||!WriteFile(f,words,used*4,&n,NULL)||n!=used*4)ExitProcess(91);CloseHandle(f);}
static void probe16(int fields){
 struct {BITMAPINFOHEADER h;DWORD masks[3];} b={0};void *bits=NULL;
 b.h.biSize=40;b.h.biWidth=2;b.h.biHeight=-2;b.h.biPlanes=1;b.h.biBitCount=16;b.h.biCompression=fields?BI_BITFIELDS:BI_RGB;
 b.masks[0]=0xf800;b.masks[1]=0x7e0;b.masks[2]=0x1f;
 HDC dc=CreateCompatibleDC(NULL);HBITMAP bitmap=CreateDIBSection(dc,(BITMAPINFO *)&b,DIB_RGB_COLORS,&bits,NULL,0);
 if(!dc||!bitmap||!bits)ExitProcess(93);HGDIOBJ old=SelectObject(dc,bitmap);HBRUSH brush=CreateSolidBrush(RGB(0x12,0x34,0x56));RECT rect={0,0,2,2};
 if(!brush||!FillRect(dc,&rect,brush))ExitProcess(94);GdiFlush();for(UINT i=0;i!=4;++i)record(((WORD *)bits)[i]);
 SelectObject(dc,old);DeleteObject(brush);DeleteObject(bitmap);DeleteDC(dc);
}
void mainCRTStartup(void){
 const char *name="adv-host-owned-user32-blockers-20261004";
 UINT message=RegisterWindowMessageA(name);record(message>=0xc000&&message<=0xffff);record(RegisterWindowMessageW(L"adv-host-owned-user32-blockers-20261004")==message);record(RegisterWindowMessageA("ADV-HOST-OWNED-USER32-BLOCKERS-20261004")==message);record(RegisterClipboardFormatA(name)==message);
 record(GlobalFindAtomA(name)==0);ATOM global=GlobalAddAtomA(name);record(GlobalFindAtomA(name)==global);GlobalDeleteAtom(global);
 UINT sizes[]={0,59,60,61,92,128};LOGFONTA a;LOGFONTW w;
 for(UINT i=0;i!=6;++i){memset(&a,0xa5,sizeof(a));SetLastError(0x1234);record(SystemParametersInfoA(SPI_GETICONTITLELOGFONT,sizes[i],&a,0));record(GetLastError());}
 memset(&a,0,sizeof(a));memset(&w,0,sizeof(w));record(SystemParametersInfoA(SPI_GETICONTITLELOGFONT,sizeof(a),&a,0));record(SystemParametersInfoW(SPI_GETICONTITLELOGFONT,sizeof(w),&w,0));record(a.lfHeight);record(a.lfWeight);record(a.lfCharSet);record(a.lfQuality);record(a.lfPitchAndFamily);for(UINT i=0;i!=32;++i)record(a.lfFaceName[i]);record(a.lfHeight==w.lfHeight&&a.lfWeight==w.lfWeight);record(w.lfFaceName[0]!=0);
 HDC dc=CreateCompatibleDC(NULL);BITMAPINFO b={0};void *bits=NULL;b.bmiHeader.biSize=40;b.bmiHeader.biWidth=4;b.bmiHeader.biHeight=-4;b.bmiHeader.biPlanes=1;b.bmiHeader.biBitCount=32;
 HBITMAP bitmap=CreateDIBSection(dc,&b,DIB_RGB_COLORS,&bits,NULL,0);if(!dc||!bitmap||!bits)ExitProcess(92);HGDIOBJ old=SelectObject(dc,bitmap);memset(bits,0xa5,64);
 RECT full={0,0,9999,9999},inner={1,1,3,3},empty={0,0,0,3},reversed={3,3,1,1};HBRUSH color=CreateSolidBrush(RGB(0x12,0x34,0x56));
 record(FillRect(dc,&full,(HBRUSH)GetStockObject(WHITE_BRUSH)));record(FillRect(dc,&inner,color));record(FillRect(dc,&inner,(HBRUSH)GetStockObject(NULL_BRUSH)));record(FillRect(dc,&empty,color));record(FillRect(dc,&reversed,color));GdiFlush();for(UINT i=0;i!=16;++i)record(((DWORD *)bits)[i]);record(GetCurrentObject(dc,OBJ_BRUSH)==GetStockObject(WHITE_BRUSH));
 DeleteObject(color);SelectObject(dc,old);DeleteObject(bitmap);DeleteDC(dc);probe16(0);probe16(1);record(0xb10c0001);save();ExitProcess(0);
}
