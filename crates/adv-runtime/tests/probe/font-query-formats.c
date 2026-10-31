/* Build: i686-w64-mingw32-gcc -O2 -Wall font-query-formats.c -o font-query-formats.exe -lgdi32 */
/* The restored native-GDI query cases, emitted for independent Windows comparison. */
#include <windows.h>
#include <stdio.h>
static FILE *out;
static void emit(const void *p,size_t size){if(fwrite(p,1,size,out)!=size)ExitProcess(90);}
static void word(DWORD v){emit(&v,4);}
static HFONT select_font(HDC dc,const WCHAR *face,int height,int charset,int italic){HFONT f=CreateFontW(height,0,0,0,400,italic,0,0,charset,0,0,0,0,face);SelectObject(dc,f);return f;}
static const WCHAR *faces[]={L"Arial",L"MS Gothic",L"Terminal"};
int main(void){
 out=fopen("font-query-formats.bin","wb");if(!out)return 1;HDC dc=CreateCompatibleDC(0);HFONT f=select_font(dc,L"Arial",-24,0,0);
 const LONG transforms[][4]={{65536,0,0,65536},{98304,16384,-32768,65536}};
 for(int t=0;t<2;t++)for(int hinted=0;hinted<2;hinted++)for(UINT format=0;format<=6;format++){
  MAT2 mat;memcpy(&mat,transforms[t],16);UINT flags=format|(hinted?0:0x100);GLYPHMETRICS gm;DWORD size=GetGlyphOutlineW(dc,'g',flags,&gm,0,0,&mat);if(size==GDI_ERROR)return 2;
  BYTE *data=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,size);GetGlyphOutlineW(dc,'g',flags,&gm,size,data,&mat);word(1);word(flags);emit(&mat,16);word(size);emit(&gm,20);emit(data,size);HeapFree(GetProcessHeap(),0,data);
 }
 DeleteObject(f);
 const BYTE offsets[]={20,21,22,23,24,25,26,27};
 const BYTE values[][4]={{0,1,7,255},{0,1,7,255},{0,1,7,255},{0,1,128,42},{0,4,8,255},{0,16,64,255},{0,3,6,255},{0,17,49,255}};
 for(int offset=0;offset<8;offset++)for(int value=0;value<4;value++){
  LOGFONTW lf={0};lf.lfHeight=-24;lf.lfWeight=400;lstrcpyW(lf.lfFaceName,L"Arial");((BYTE*)&lf)[offsets[offset]]=values[offset][value];f=CreateFontIndirectW(&lf);SelectObject(dc,f);TEXTMETRICA tm;memset(&tm,0,sizeof(tm));word(2);word(offsets[offset]);word(values[offset][value]);word(GetTextMetricsA(dc,&tm));emit(&tm,sizeof(tm));DeleteObject(f);
 }
 const int heights[]={-12,-25,-38};
 for(int face=0;face<3;face++)for(int height=0;height<3;height++)for(int italic=0;italic<2;italic++){
  f=select_font(dc,faces[face],heights[height],0,italic);WORD indices[5]={0,0,0,0,65535};GetGlyphIndicesW(dc,L"Afx?",4,indices,0);
  const DWORD firsts[]={0,0xffffffff,65535,65537,0},counts[]={4,5,1,1,0};
  for(int test=0;test<5;test++){
   int widths[5];ABC abc[5];memset(widths,0xcd,sizeof(widths));memset(abc,0xcd,sizeof(abc));WORD *array=test==1?indices:0;
   word(3);word(face);word(heights[height]);word(italic);word(test);emit(indices,sizeof(indices));word(GetCharWidthI(dc,firsts[test],counts[test],array,widths));emit(widths,sizeof(widths));word(GetCharABCWidthsI(dc,firsts[test],counts[test],array,abc));emit(abc,sizeof(abc));
  }DeleteObject(f);
 }
 static const WCHAR *inputs[]={L"AV fi",L"AV fi",L"AV fi",L"abc",L"\x3042\x3044\x3046",L"\x4e2d\x6587",L"\x05d0\x05d1\x05d2 abc",L"\x0633\x0644\x0627\x0645"};
 const DWORD flags[]={0,8,0x100000,2,0,0,2,2|0x20};const int max[]={100,100,30,100,100,100,100,100};const DWORD capacities[]={0,2,16};
 for(int face=0;face<3;face++)for(int wide=0;wide<2;wide++)for(int test=0;test<8;test++){
  if(!wide&&face==0&&test>=4)continue;
  UINT cp=face==2?936:932;int charset=face==1?128:face==2?134:0;
  f=select_font(dc,face==2?L"Microsoft YaHei":faces[face],-24,charset,0);const WCHAR *input=inputs[test];char ansi[64];int length=WideCharToMultiByte(cp,0,input,lstrlenW(input),ansi,sizeof(ansi),0,0);
  for(int capacity=0;capacity<3;capacity++){
   WCHAR string[16],glyphs[16];BYTE astring[16],classes[16];UINT order[16];int dx[16],caret[16];memset(string,0xcd,sizeof(string));memset(astring,0xcd,sizeof(astring));memset(classes,0xcd,sizeof(classes));memset(order,0xcd,sizeof(order));memset(dx,0xcd,sizeof(dx));memset(caret,0xcd,sizeof(caret));memset(glyphs,0xcd,sizeof(glyphs));
   GCP_RESULTSW result={sizeof(result),wide?string:(WCHAR*)astring,order,dx,caret,(LPSTR)classes,glyphs,capacities[capacity],0x12345678};
   DWORD extent=wide?GetCharacterPlacementW(dc,input,lstrlenW(input),max[test],&result,flags[test]):GetCharacterPlacementA(dc,ansi,length,max[test],(GCP_RESULTSA*)&result,flags[test]);
   word(4);word(face);word(wide);word(test);word(capacities[capacity]);word(extent);word(result.nGlyphs);word(result.nMaxFit);if(wide)emit(string,sizeof(string));else emit(astring,sizeof(astring));emit(order,sizeof(order));emit(dx,sizeof(dx));emit(caret,sizeof(caret));emit(classes,sizeof(classes));emit(glyphs,sizeof(glyphs));
  }DeleteObject(f);
 }
 word(0);DeleteDC(dc);fclose(out);return 0;
}
