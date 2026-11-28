/* Build: i686-w64-mingw32-gcc -O2 -Wall text-records.c -o text-records.exe -lgdi32 -luser32 */
/* Authored text record oracle (WINE-UNIX-8): no sample input or visible windows.
 * Measures without drawing, draws known lines into a DIB through TextOutW,
 * Shift_JIS ExtTextOutA, a two-line DrawTextW and glyph indices, then fetches
 * one glyph. Outputs text-records.bin: the glyph buffer address, its size and
 * the two glyph indices. */
#include <windows.h>
static BYTE glyph[8192];
int main(void){
 HDC dc=CreateCompatibleDC(0);BITMAPINFO info={0};info.bmiHeader.biSize=40;info.bmiHeader.biWidth=160;info.bmiHeader.biHeight=-96;info.bmiHeader.biPlanes=1;info.bmiHeader.biBitCount=32;DWORD *pixels=0;HBITMAP bitmap=CreateDIBSection(dc,&info,DIB_RGB_COLORS,(void**)&pixels,0,0);if(!dc||!bitmap)ExitProcess(1);SelectObject(dc,bitmap);
 HFONT font=CreateFontW(-16,0,0,0,400,0,0,0,SHIFTJIS_CHARSET,0,0,ANTIALIASED_QUALITY,0,L"MS Gothic");if(!font)ExitProcess(1);SelectObject(dc,font);SetBkMode(dc,TRANSPARENT);
 SIZE size;if(!GetTextExtentPoint32W(dc,L"\x3042Z",2,&size))ExitProcess(1);
 if(!TextOutW(dc,2,2,L"\x3042" L"A",2))ExitProcess(1);
 if(!ExtTextOutA(dc,2,20,0,0,"\x82\xa0" "B",3,0))ExitProcess(1);
 RECT r={2,38,158,94};if(!DrawTextW(dc,L"C\r\nD",-1,&r,0))ExitProcess(1);
 WORD indices[2];if(GetGlyphIndicesW(dc,L"AB",2,indices,0)!=2)ExitProcess(1);
 if(!ExtTextOutW(dc,80,2,ETO_GLYPH_INDEX,0,(LPCWSTR)indices,2,0))ExitProcess(1);
 GLYPHMETRICS metrics;MAT2 identity={{0,1},{0,0},{0,0},{0,1}};
 DWORD bytes=GetGlyphOutlineW(dc,0x3042,GGO_GRAY8_BITMAP,&metrics,sizeof glyph,glyph,&identity);if(bytes==GDI_ERROR||!bytes)ExitProcess(1);
 GdiFlush();
 DWORD out[4]={(DWORD)(UINT_PTR)glyph,bytes,indices[0],indices[1]};HANDLE file=CreateFileA("text-records.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);DWORD written;if(file==INVALID_HANDLE_VALUE||!WriteFile(file,out,sizeof out,&written,0)||written!=sizeof out)ExitProcess(2);CloseHandle(file);ExitProcess(0);
}
