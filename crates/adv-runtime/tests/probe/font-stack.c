/* Authored text-stack records. Only memory DCs and owned DIB sections. */
#include <windows.h>
#include <stdio.h>
static FILE *output;
static void emit(const void *p,size_t n){if(fwrite(p,1,n,output)!=n)ExitProcess(90);}
static void word(DWORD n){emit(&n,4);}
static const WCHAR paragraph[]={0x3042,'A',' ',0x3042,'B',' ','&','C','\t',0x3042,'D',' ',0x3042,'E',0};
static int CALLBACK enumeration(const LOGFONTW *lf,const TEXTMETRICW *tm,DWORD kind,LPARAM p){(void)lf;(void)p;word(kind);word(tm->tmPitchAndFamily);word(tm->tmCharSet);return 0;}
static HFONT font(int angle,const WCHAR *face){return CreateFontW(-24,0,angle,angle,400,0,0,0,SHIFTJIS_CHARSET,OUT_TT_ONLY_PRECIS,0,ANTIALIASED_QUALITY,0,face);}
int main(void){
 output=fopen("font-stack.bin","wb");if(!output)return 1;
 HDC dc=CreateCompatibleDC(0);HFONT f=font(0,L"ADV Japanese CFF Fixture");HGDIOBJ old=SelectObject(dc,f);
 LOGFONTW query={0};query.lfCharSet=SHIFTJIS_CHARSET;lstrcpyW(query.lfFaceName,L"ADV Japanese CFF Fixture");EnumFontFamiliesExW(dc,&query,enumeration,0,0);
 TEXTMETRICW tm;word(GetTextMetricsW(dc,&tm));emit(&tm,sizeof(tm));
 int n=lstrlenW(paragraph),fit=0,dx[32];SIZE size;
 word(GetTextExtentExPointW(dc,paragraph,n,100,&fit,dx,&size));word(fit);emit(&size,sizeof(size));emit(dx,n*4);
 for(int i=0;i<n;i++){int width;ABC abc;GLYPHMETRICS gm;MAT2 m={{0,1},{0,0},{0,0},{0,1}};GetCharWidth32W(dc,paragraph[i],paragraph[i],&width);GetCharABCWidthsW(dc,paragraph[i],paragraph[i],&abc);GetGlyphOutlineW(dc,paragraph[i],GGO_METRICS,&gm,0,0,&m);word(width);emit(&abc,sizeof(abc));word(gm.gmCellIncX);}
 for(int format=2;format<=6;format+=format==2?1:3){GLYPHMETRICS gm;MAT2 m={{0,1},{0,0},{0,0},{0,1}};DWORD size=GetGlyphOutlineW(dc,0x3042,format,&gm,0,0,&m);if(size==GDI_ERROR)return 3;BYTE *bytes=HeapAlloc(GetProcessHeap(),0,size);word(format);word(size);emit(&gm,sizeof(gm));if(GetGlyphOutlineW(dc,0x3042,format,&gm,size,bytes,&m)==GDI_ERROR)return 4;emit(bytes,size);HeapFree(GetProcessHeap(),0,bytes);}
 const int depths[]={1,4,8,16,24,32};
 for(int test=0;test<10;test++){
  int bpp=depths[test<6?test:5],pitch=((128*bpp+31)/32)*4;struct {BITMAPINFOHEADER header;RGBQUAD colors[256];} info={0};void *bits;
  info.header.biSize=40;info.header.biWidth=128;info.header.biHeight=-96;info.header.biPlanes=1;info.header.biBitCount=bpp;
  if(bpp<=8){info.header.biClrUsed=1u<<bpp;for(unsigned i=0;i<(1u<<bpp);i++){BYTE v=i*255/((1u<<bpp)-1);info.colors[i].rgbRed=v;info.colors[i].rgbGreen=v;info.colors[i].rgbBlue=v;}}
  HBITMAP bitmap=CreateDIBSection(dc,(BITMAPINFO*)&info,0,&bits,0,0);HGDIOBJ previous=SelectObject(dc,bitmap);memset(bits,0x23,pitch*96);SetBkMode(dc,TRANSPARENT);SetTextColor(dc,RGB(220,180,140));SetBkColor(dc,RGB(30,50,70));
  HFONT changed=0;if(test==6){changed=font(900,L"ADV Japanese CFF Fixture");SelectObject(dc,changed);}if(test==7){changed=font(0,L"@ADV Japanese CFF Fixture");SelectObject(dc,changed);}
  if(test==8){SetTextAlign(dc,TA_UPDATECP);MoveToEx(dc,12,14,0);}
  RECT r={5,6,100,80};WCHAR text[]={0x3042,0x3042,0};
  word(ExtTextOutW(dc,24,32,test==9?ETO_OPAQUE|ETO_CLIPPED:0,&r,text,2,0));GdiFlush();POINT cp;GetCurrentPositionEx(dc,&cp);emit(&cp,sizeof(cp));word(bpp);word(pitch*96);emit(bits,pitch*96);
  SetTextAlign(dc,0);if(changed){SelectObject(dc,f);DeleteObject(changed);}SelectObject(dc,previous);DeleteObject(bitmap);
 }
 for(int test=0;test<5;test++){
  BITMAPINFO info={{40,128,-96,1,32,BI_RGB}};void *bits;HBITMAP bm=CreateDIBSection(dc,&info,0,&bits,0,0);HGDIOBJ previous=SelectObject(dc,bm);memset(bits,0x23,128*96*4);
  UINT flags=DT_WORDBREAK|DT_EXPANDTABS;if(test==1)flags|=DT_END_ELLIPSIS;if(test==2)flags|=DT_CALCRECT;if(test==3)flags|=DT_CENTER;if(test==4)flags|=DT_EDITCONTROL;
  RECT r={3,4,83,90};word(flags);word(DrawTextW(dc,paragraph,n,&r,flags));emit(&r,sizeof(r));GdiFlush();emit(bits,128*96*4);SelectObject(dc,previous);DeleteObject(bm);
 }
 SelectObject(dc,old);DeleteObject(f);DeleteDC(dc);fclose(output);return 0;
}
