/* Authored private, hidden font resource lifetime oracle. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
int _fltused;
void *memset(void *p,int c,unsigned n){unsigned char *q=p;while(n--)*q++=(unsigned char)c;return p;}
static DWORD words[64];static UINT count,enumerated;
static void record(DWORD value){words[count++]=value;}
/* These APIs do not define extended error information. Compare results only. */
static void result(DWORD value){record(value);}
static int CALLBACK family(const LOGFONTW *font,const TEXTMETRICW *metric,DWORD type,LPARAM param){(void)font;(void)metric;(void)type;(void)param;enumerated++;return 1;}
static void save(void){DWORD wrote;HANDLE f=CreateFileA("font-resource-private.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);if(f==INVALID_HANDLE_VALUE)ExitProcess(90);if(!WriteFile(f,words,count*4,&wrote,NULL)||wrote!=count*4)ExitProcess(91);CloseHandle(f);}
int main(void){
    const DWORD flags=FR_PRIVATE|FR_NOT_ENUM;
    HDC dc;HFONT font,old;GLYPHMETRICS metrics;MAT2 transform={{0,1},{0,0},{0,0},{0,1}};
    LOGFONTW logical={0};LOGFONTA selected={0};UINT i;const WCHAR *face=L"ADV Resource Fixture";
    const char *ansi_face="ADV Resource Fixture";
    SetLastError(0x12345678);result(AddFontResourceExW(L"font-resource.ttf",flags,NULL));
    SetLastError(0x12345678);result(AddFontResourceExA("font-resource.ttf",flags,NULL));
    dc=CreateCompatibleDC(NULL);if(!dc)ExitProcess(2);
    for(i=0;face[i];i++)logical.lfFaceName[i]=face[i];logical.lfCharSet=DEFAULT_CHARSET;
    EnumFontFamiliesExW(dc,&logical,(FONTENUMPROCW)family,0,0);record(enumerated);
    for(i=0;ansi_face[i];i++)selected.lfFaceName[i]=ansi_face[i];
    selected.lfHeight=-40;selected.lfWeight=FW_NORMAL;selected.lfQuality=ANTIALIASED_QUALITY;selected.lfCharSet=DEFAULT_CHARSET;
    font=CreateFontIndirectA(&selected);
    if(!font)ExitProcess(3);old=SelectObject(dc,font);
    record(GetGlyphOutlineW(dc,'B',GGO_METRICS,&metrics,0,NULL,&transform));record(metrics.gmCellIncX);
    SetLastError(0x12345678);result(RemoveFontResourceW(L"font-resource.ttf"));
    SetLastError(0x12345678);result(RemoveFontResourceExA("font-resource.ttf",flags,NULL));
    record(GetGlyphOutlineW(dc,'B',GGO_METRICS,&metrics,0,NULL,&transform));record(metrics.gmCellIncX);
    SelectObject(dc,old);DeleteObject(font);DeleteDC(dc);
    SetLastError(0x12345678);result(RemoveFontResourceExW(L"font-resource.ttf",flags,NULL));
    SetLastError(0x12345678);result(RemoveFontResourceExW(L"font-resource.ttf",flags,NULL));
    SetLastError(0x12345678);result(AddFontResourceExW(L"missing.ttf",flags,NULL));
    record(0xf0170030);save();ExitProcess(0);
}
