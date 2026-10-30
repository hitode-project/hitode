/* Headless session/first-glyph and guest-plus-host drawing benchmark. */
#include <windows.h>
#include <stdio.h>
int main(void){
 HDC dc=CreateCompatibleDC(0);HFONT f=CreateFontW(-24,0,0,0,400,0,0,0,SHIFTJIS_CHARSET,0,0,ANTIALIASED_QUALITY,0,L"MS Gothic");SelectObject(dc,f);GLYPHMETRICS gm;MAT2 m={{0,1},{0,0},{0,0},{0,1}};if(GetGlyphOutlineW(dc,0x3042,GGO_GRAY8_BITMAP,&gm,0,0,&m)==GDI_ERROR)return 2;
 FILE *mode=fopen("draw-mode","rb");if(!mode){DeleteDC(dc);DeleteObject(f);return 0;}fclose(mode);
 BITMAPINFO info={{40,512,-256,1,32,BI_RGB}};void *bits;HBITMAP bm=CreateDIBSection(dc,&info,0,&bits,0,0);SelectObject(dc,bm);SetBkMode(dc,TRANSPARENT);SetTextColor(dc,RGB(240,220,200));
 static const WCHAR text[]={0x3042,0x3044,0x3046,0x3048,0x304a,' ','a','b','c',' ',0x3042,0x3044,0x3046,0};
 LARGE_INTEGER start,end,freq;QueryPerformanceFrequency(&freq);QueryPerformanceCounter(&start);
 for(int i=0;i<2000;i++)if(!TextOutW(dc,i%240,i%160,text,13))return 3;
 for(int i=0;i<200;i++){RECT r={0,0,180,256};if(!DrawTextW(dc,text,13,&r,DT_WORDBREAK))return 4;}
 GdiFlush();QueryPerformanceCounter(&end);printf("%.9f\n",(double)(end.QuadPart-start.QuadPart)/(double)freq.QuadPart);DeleteDC(dc);DeleteObject(bm);DeleteObject(f);return 0;
}
