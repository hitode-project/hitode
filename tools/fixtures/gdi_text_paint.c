#define WIN32_LEAN_AND_MEAN
#include <windows.h>
/* A bounded, silent fixture: only private memory DCs and owned DIB sections. */
static HANDLE output;
static void emit(const void *data,DWORD size) {DWORD n;if(!WriteFile(output,data,size,&n,NULL)||n!=size)ExitProcess(90);}
void mainCRTStartup(void) {
    static const char ansi[]={'W','i','A','V',0,'x',(char)0x82,(char)0xa0,(char)0x82};
    static const WCHAR wide[]={'W','i',0,'x',0x3042,0x3044,0xd83d,0xde00};
    static const UINT aligns[]={0,2,6,8,10,14,24,26,30};
    DWORD records=0; int format,up,style,align;
    output=CreateFileA("gdi-text-paint.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);if(output==INVALID_HANDLE_VALUE)ExitProcess(1);
    for(format=0;format<4;format++)for(up=0;up<2;up++)for(style=0;style<4;style++)for(align=0;align<9;align++) {
        struct {BITMAPINFOHEADER bmiHeader; RGBQUAD bmiColors[3];} info={0}; void *bits; HDC dc=CreateCompatibleDC(NULL); HBITMAP bitmap,oldbitmap; HFONT font,oldfont; LOGFONTA lf={0};
        DWORD header[10],n; int i; HPALETTE pal=NULL,oldpal=NULL;
        struct {WORD version,count; PALETTEENTRY entries[3];} palette={0x300,3,{{0,0,0,0},{193,47,91,0},{21,117,169,0}}}; RECT rect={13,9,76,47};
        int bpp=format==0||format==3?16:format==1?24:32; int pitch=((96*bpp/8)+3)&~3;
        info.bmiHeader.biSize=40;info.bmiHeader.biWidth=96;info.bmiHeader.biHeight=up?-56:56;info.bmiHeader.biPlanes=1;info.bmiHeader.biBitCount=(WORD)bpp;
        if(format==3){info.bmiHeader.biCompression=BI_BITFIELDS;((DWORD*)info.bmiColors)[0]=0xf800;((DWORD*)info.bmiColors)[1]=0x7e0;((DWORD*)info.bmiColors)[2]=0x1f;}
        bitmap=CreateDIBSection(dc,(BITMAPINFO*)&info,DIB_RGB_COLORS,&bits,NULL,0);if(!bitmap||!dc)ExitProcess(2);
        oldbitmap=(HBITMAP)SelectObject(dc,bitmap);
        /* Byte pattern exposes transparent background, alpha and row direction. */
        for(i=0;i<pitch*56;i++)((BYTE*)bits)[i]=(BYTE)(i*37+19);
        if(style==2)font=(HFONT)GetStockObject(SYSTEM_FONT);
        else {
            lf.lfHeight=-19;lf.lfWeight=400;lf.lfItalic=style==0;lf.lfUnderline=style==0;
            lf.lfCharSet=style==1?SHIFTJIS_CHARSET:ANSI_CHARSET;
            lf.lfQuality=style==0?CLEARTYPE_QUALITY:NONANTIALIASED_QUALITY;
            if(style!=1){lf.lfFaceName[0]='A';lf.lfFaceName[1]='r';lf.lfFaceName[2]='i';lf.lfFaceName[3]='a';lf.lfFaceName[4]='l';}
            else {lf.lfFaceName[0]='M';lf.lfFaceName[1]='S';lf.lfFaceName[2]=' ';lf.lfFaceName[3]='G';lf.lfFaceName[4]='o';lf.lfFaceName[5]='t';lf.lfFaceName[6]='h';lf.lfFaceName[7]='i';lf.lfFaceName[8]='c';}
            font=CreateFontIndirectA(&lf);
        }
        if(!font)ExitProcess(3);oldfont=(HFONT)SelectObject(dc,font);
        if(style==3){pal=CreatePalette((LOGPALETTE*)&palette);if(!pal)ExitProcess(9);oldpal=SelectPalette(dc,pal,FALSE);SetTextColor(dc,PALETTEINDEX(1));SetBkColor(dc,PALETTEINDEX(2));}
        else {SetTextColor(dc,RGB(193,47,91));SetBkColor(dc,RGB(21,117,169));}SetBkMode(dc,align%2?OPAQUE:TRANSPARENT);SetTextAlign(dc,aligns[align]);
        header[0]=format==3?565:(DWORD)bpp;header[1]=(DWORD)up;header[2]=(DWORD)style;header[3]=aligns[align];header[4]=align%2?2:1;header[5]=(DWORD)(align%4);
        header[6]=style==3&&(align%4==0||align%4==2)?0:style==1?9:6;header[7]=96;header[8]=56;header[9]=(DWORD)(pitch*56);
        switch(align%4) {
        case 0:n=TextOutA(dc,48,28,ansi,(int)header[6]);break;
        case 1:n=TextOutW(dc,48,28,wide,8);break;
        case 2:n=ExtTextOutA(dc,48,28,ETO_OPAQUE|ETO_CLIPPED,&rect,ansi,header[6],NULL);break;
        default:n=ExtTextOutW(dc,48,28,ETO_CLIPPED,&rect,wide,8,NULL);break;
        }
        if(!n)ExitProcess(4);if(!GdiFlush())ExitProcess(5);
        emit(header,sizeof(header));emit(bits,header[9]);records++;
        if(pal){SelectPalette(dc,oldpal,FALSE);if(!DeleteObject(pal))ExitProcess(10);}
        SelectObject(dc,oldfont);SelectObject(dc,oldbitmap);if(style!=2&&!DeleteObject(font))ExitProcess(6);if(!DeleteObject(bitmap)||!DeleteDC(dc))ExitProcess(7);
    }
    CloseHandle(output);ExitProcess(records==288?0:8);
}
