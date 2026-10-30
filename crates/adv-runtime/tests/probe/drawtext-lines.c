/* Build: i686-w64-mingw32-gcc -O2 -Wall drawtext-lines.c -o drawtext-lines.exe -lgdi32 -luser32 */
/* Authored A/W paragraph oracle: no sample input or visible windows.
 * Outputs drawtext-lines.bin with return values, unchanged rectangles and
 * RGB hashes of a selected DIB; native cursor and settings are untouched. */
#include <windows.h>
static DWORD values[1024],used;
static void put(DWORD value){values[used++]=value;}
int main(void){
 HDC dc=CreateCompatibleDC(0);BITMAPINFO info={0};info.bmiHeader.biSize=40;info.bmiHeader.biWidth=96;info.bmiHeader.biHeight=-64;info.bmiHeader.biPlanes=1;info.bmiHeader.biBitCount=32;DWORD *pixels=0;HBITMAP bitmap=CreateDIBSection(dc,&info,DIB_RGB_COLORS,(void**)&pixels,0,0);if(!dc||!bitmap)ExitProcess(1);HGDIOBJ old=SelectObject(dc,bitmap);
 HFONT font=CreateFontA(-16,0,0,0,400,0,0,0,DEFAULT_CHARSET,0,0,NONANTIALIASED_QUALITY,0,"Arial");HGDIOBJ previous=SelectObject(dc,font);SetTextColor(dc,RGB(240,220,200));SetBkMode(dc,TRANSPARENT);
 const char *a[]={"AB","AB\r\nCD","AB\nCD","AB\rCD","AB\r\nCD\r\n","AB&&CD\r\nEF","AB\r\nCD","AB\r\nCD\r\n\r\n","AB\r\n","\r\n","AB\r\nCD"};
 const WCHAR *w[]={L"AB",L"AB\r\nCD",L"AB\nCD",L"AB\rCD",L"AB\r\nCD\r\n",L"AB&&CD\r\nEF",L"AB\r\nCD",L"AB\r\nCD\r\n\r\n",L"AB\r\n",L"\r\n",L"AB\r\nCD"};
 for(UINT front=0;front<2;front++)for(UINT n=0;n<11;n++)for(UINT clipped=0;clipped<2;clipped++){
  for(UINT p=0;p<96*64;p++)pixels[p]=0x00335577;
  RECT r={3,4,90,clipped?18:60};UINT flags=n==6?DT_EXTERNALLEADING:0;if(n==5)flags|=DT_NOPREFIX;if(n==10)flags|=DT_NOCLIP;
  put(front?DrawTextW(dc,w[n],-1,&r,flags):DrawTextA(dc,a[n],-1,&r,flags));GdiFlush();put(r.left);put(r.top);put(r.right);put(r.bottom);
  DWORD hash=2166136261u,changed=0;for(UINT p=0;p<96*64;p++){DWORD rgb=pixels[p]&0xffffff;hash=(hash^rgb)*16777619u;if(rgb!=0x00335577)changed++;}put(hash);put(changed);
 }
 SelectObject(dc,previous);DeleteObject(font);SelectObject(dc,old);DeleteObject(bitmap);DeleteDC(dc);HANDLE file=CreateFileA("drawtext-lines.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD bytes;if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,used*4,&bytes,0)||bytes!=used*4)ExitProcess(2);CloseHandle(file);ExitProcess(0);
}
