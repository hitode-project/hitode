/* Authored FreeType/GDI byte oracle. Build against FreeType 2.13.2 headers:
 * i686-w64-mingw32-gcc -O1 -I <freetype>/include -o freetype.exe freetype.c -lgdi32
 * No FreeType import library: every entry comes from adv-unix's dlopen/dlsym. */
#include <windows.h>
#include <stdio.h>
#include <ft2build.h>
#include FT_FREETYPE_H
#include FT_OUTLINE_H
#include FT_GLYPH_H
#include FT_TRUETYPE_TABLES_H
#include FT_SFNT_NAMES_H
#include FT_TRUETYPE_DRIVER_H
#include FT_MODULE_H
#include FT_LCD_FILTER_H
#include FT_TRIGONOMETRY_H
#include FT_WINFONTS_H

#define FUNCTIONS(X) \
 X(FT_Init_FreeType) X(FT_Done_Face) X(FT_New_Face) X(FT_New_Memory_Face) \
 X(FT_Library_Version) X(FT_Library_SetLcdFilter) X(FT_Get_TrueType_Engine_Type) \
 X(FT_Get_Char_Index) X(FT_Get_First_Char) X(FT_Get_Next_Char) X(FT_Set_Charmap) \
 X(FT_Get_Sfnt_Name) X(FT_Get_Sfnt_Name_Count) X(FT_Get_Sfnt_Table) \
 X(FT_Get_WinFNT_Header) X(FT_Load_Sfnt_Table) X(FT_Set_Pixel_Sizes) \
 X(FT_Load_Glyph) X(FT_Render_Glyph) X(FT_MulDiv) X(FT_MulFix) \
 X(FT_Matrix_Multiply) X(FT_Vector_Transform) X(FT_Vector_Length) X(FT_Vector_Unit) \
 X(FT_Outline_Transform) X(FT_Outline_Translate) X(FT_Outline_Embolden) \
 X(FT_Outline_Get_CBox) X(FT_Outline_Get_Bitmap) X(FT_Property_Set)
#define DECLARE(f) static typeof(f) *p##f;
FUNCTIONS(DECLARE)
static HANDLE output;
static unsigned records;
static const char *unix_font;
static void bytes(const void *data, unsigned length)
{
    DWORD written=0;
    if (!length) return;
    if (!WriteFile(output, data, length, &written, NULL) || written != length) { printf("write record=%u len=%u written=%lu error=%lu\n", records, length, written, GetLastError()); ExitProcess(4); }
}
static void number(DWORD n) { bytes(&n, sizeof(n)); }
static void record(const void *data, unsigned length)
{
    number(records++); number(length); bytes(data, length);
}
static DWORD seed = 0x53ea7129;
static DWORD random_value(void) { seed = seed * 1664525 + 1013904223; return seed; }
static void dump_outline(const FT_Outline *outline)
{
    number(outline->n_contours); number(outline->n_points); number(outline->flags);
    record(outline->points, outline->n_points * sizeof(*outline->points));
    record(outline->tags, outline->n_points);
    record(outline->contours, outline->n_contours * sizeof(*outline->contours));
}
static void dump_slot(FT_Face face)
{
    FT_GlyphSlot slot = face->glyph;
    record(&face->size->metrics, sizeof(face->size->metrics));
    record(&slot->metrics, sizeof(slot->metrics));
    record(&slot->linearHoriAdvance, 4 * sizeof(FT_Long));
    number(slot->format); number(slot->bitmap_left); number(slot->bitmap_top);
    number(slot->bitmap.rows); number(slot->bitmap.width); number(slot->bitmap.pitch);
    number(slot->bitmap.num_grays); number(slot->bitmap.pixel_mode); number(slot->bitmap.palette_mode);
    number(slot->bitmap.buffer != NULL);
    record(slot->bitmap.buffer, slot->bitmap.buffer ? slot->bitmap.rows * abs(slot->bitmap.pitch) : 0);
    dump_outline(&slot->outline);
}
static void test_math(void)
{
    static const LONG edge[][3] = {
        {0,0,0}, {1,1,0}, {-1,1,0}, {65536,65536,65536},
        {0x7fffffff,65536,1}, {(LONG)0x80000000,1,1},
        {0x7fffffff,0x7fffffff,-1}, {(LONG)0x80000000,(LONG)0x80000000,65536},
        {(LONG)0x80000000,-1,1}, {(LONG)0x80000000,2,1},
        {(LONG)0x80000000,3,1}, {(LONG)0x80000000,4,1},
        {(LONG)0x80000000,0,1}, {0,(LONG)0x80000000,1},
        {1,(LONG)0x80000000,1}, {2,(LONG)0x80000000,1},
        {3,(LONG)0x80000000,1}, {0x7fffffff,(LONG)0x80000000,1}
    };
    unsigned i;
    for (i = 0; i < 128; i++)
    {
        LONG a = (LONG)(random_value() % 200001) - 100000;
        LONG b = (LONG)(random_value() % 200001) - 100000;
        LONG c = i == 0 ? 0 : (LONG)(random_value() % 200001) - 100000;
        if (i < sizeof(edge)/sizeof(*edge)) { a=edge[i][0]; b=edge[i][1]; c=edge[i][2]; }
        FT_Vector vector = {a,b};
        FT_Matrix left = {65536, b, a, 65536}, right = {32768, a, b, 65536};
        number(pFT_MulDiv(a,b,c)); number(pFT_MulFix(a,b));
        number(pFT_Vector_Length(&vector));
        pFT_Vector_Transform(&vector, &left); record(&vector, sizeof(vector));
        pFT_Vector_Unit(&vector, a); record(&vector, sizeof(vector));
        pFT_Matrix_Multiply(&left, &right); record(&right, sizeof(right));
    }
}
static void test_outlines(FT_Library library)
{
    unsigned i;
    for (i = 0; i < 48; i++)
    {
        FT_Vector points[5] = {{0,0},{256,32},{384,320},{128,448},{-64,192}};
        char tags[5] = {1,1,0,1,1};
        short contours[1] = {4};
        FT_Outline outline = {1,5,points,tags,contours,0};
        FT_BBox box = {0};
        FT_Matrix matrix = {65536, (LONG)(random_value()%16001)-8000, 0,65536};
        BYTE bitmap_data[256];
        FT_Bitmap bitmap = {16,16,(i%2 ? -16 : 16),bitmap_data,256,FT_PIXEL_MODE_GRAY,0,NULL};
        memset(bitmap_data,0x37,sizeof(bitmap_data));
        if (i%3 == 0) { bitmap.pixel_mode = FT_PIXEL_MODE_MONO; bitmap.pitch = 4; }
        if (i%5 == 0) outline.n_points = outline.n_contours = 0;
        if (i == 43) bitmap.rows = 0;
        if (i == 44) bitmap.width = 0;
        if (i == 46) outline.tags[0] = 2; /* invalid cubic start */
        pFT_Outline_Transform(&outline,&matrix);
        pFT_Outline_Translate(&outline,128,128);
        number(pFT_Outline_Embolden(&outline,(i%4)*16));
        pFT_Outline_Get_CBox(&outline,&box); record(&box,sizeof(box));
        dump_outline(&outline);
        number(pFT_Outline_Get_Bitmap(library,&outline,&bitmap));
        record(bitmap_data,sizeof(bitmap_data));
    }
    {
        FT_Vector points[2]={{0,0},{0x2000000,64}};
        char tags[2]={1,1}; short contours[1]={1};
        FT_Outline outline={1,2,points,tags,contours,0};
        BYTE data[16]={0};
        FT_Bitmap bitmap={4,4,4,data,256,FT_PIXEL_MODE_GRAY,0,NULL};
        number(pFT_Outline_Get_Bitmap(library,&outline,&bitmap)); record(data,sizeof(data));
        points[1].x=64; contours[0]=0;
        number(pFT_Outline_Get_Bitmap(library,&outline,&bitmap)); record(data,sizeof(data));
    }
    number(pFT_Outline_Embolden(NULL,64));
    number(pFT_Outline_Get_Bitmap(NULL,NULL,NULL));
}
static void test_face(FT_Library library, const char *path, int memory)
{
    FT_Face face = NULL;
    DWORD length, read;
    BYTE *data = NULL;
    unsigned i, j, index, code;
    unsigned sizes[] = {0,1,9,16,23,48};
    unsigned codes[] = {0,32,65,66,0x3042,0x4e00,0xffff};
    unsigned flags[] = {FT_LOAD_DEFAULT,FT_LOAD_NO_HINTING,FT_LOAD_TARGET_MONO,FT_LOAD_RENDER};
    HANDLE file = CreateFileA(path,GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(5);
    length = GetFileSize(file,NULL);
    data = HeapAlloc(GetProcessHeap(),0,length);
    ReadFile(file,data,length,&read,NULL); CloseHandle(file);
    {
        FT_Face query=NULL;
        number(pFT_New_Memory_Face(library,NULL,0,0,&query));
        number(pFT_New_Memory_Face(library,data,0,0,&query));
        number(pFT_New_Memory_Face(library,data,length,0,NULL));
        number(pFT_New_Memory_Face(library,data,length,-1,NULL));
        number(pFT_New_Face(library,NULL,0,&query));
        number(pFT_New_Face(library,unix_font,0,NULL));
        number(pFT_New_Face(library,unix_font,-1,&query));
        number(query->num_faces); number(pFT_Set_Pixel_Sizes(query,0,16));
        number(pFT_Done_Face(query));
    }
    if (memory) number(pFT_New_Memory_Face(library,data,length,0,&face));
    else number(pFT_New_Face(library,unix_font,0,&face)); /* C:\ -> Unix absolute path is supplied below. */
    if (!face) ExitProcess(6);
    {
        FT_Face other=NULL;
        FT_CharMap original=face->charmap;
        number(pFT_New_Memory_Face(library,data,length,0,&other));
        number(pFT_Set_Charmap(face,other->charmap));
        number(face->charmap==original);
        number(pFT_Set_Charmap(face,NULL));
        number(pFT_Done_Face(other));
    }
    number(face->num_faces); number(face->face_index); number(face->face_flags); number(face->style_flags); number(face->num_glyphs);
    record(face->family_name,strlen(face->family_name)+1); record(face->style_name,strlen(face->style_name)+1);
    record(&face->bbox,sizeof(face->bbox)); record(&face->units_per_EM,16);
    number(face->num_charmaps);
    for(i=0;i<(unsigned)face->num_charmaps;i++)
    {
        record(&face->charmaps[i]->encoding,8);
        number(pFT_Set_Charmap(face,face->charmaps[i]));
    }
    code=pFT_Get_First_Char(face,&index);
    for(i=0;i<12;i++) { number(code); number(index); code=pFT_Get_Next_Char(face,code,&index); }
    for(i=0;i<pFT_Get_Sfnt_Name_Count(face);i++)
    {
        FT_SfntName name={0};
        number(pFT_Get_Sfnt_Name(face,i,&name)); record(&name,8); record(name.string,name.string_len);
    }
    for(i=0;i<4;i++)
    {
        unsigned tags[]={ft_sfnt_head,ft_sfnt_os2,ft_sfnt_hhea,ft_sfnt_post};
        void *table=pFT_Get_Sfnt_Table(face,tags[i]);
        unsigned table_sizes[]={sizeof(TT_Header),sizeof(TT_OS2),sizeof(TT_HoriHeader),sizeof(TT_Postscript)};
        number(table != NULL);
        if(table) record(table,table_sizes[i]);
    }
    {
        FT_ULong len=0;
        BYTE table[256];
        FT_WinFNT_HeaderRec header;
        memset(&header,0xcc,sizeof(header));
        number(pFT_Get_WinFNT_Header(face,&header)); record(&header,sizeof(header));
        number(pFT_Load_Sfnt_Table(face,FT_MAKE_TAG('h','e','a','d'),0,NULL,&len)); number(len);
        memset(table,0xcc,sizeof(table)); len=sizeof(table);
        number(pFT_Load_Sfnt_Table(face,FT_MAKE_TAG('h','e','a','d'),0,table,&len)); number(len); record(table,sizeof(table));
    }
    for(i=0;i<sizeof(sizes)/sizeof(*sizes);i++)
    {
        number(pFT_Set_Pixel_Sizes(face,0,sizes[i]));
        for(j=0;j<28;j++)
        {
            FT_Matrix matrix={65536,8192,0,65536};
            index=pFT_Get_Char_Index(face,codes[j%7]); number(index);
            number(pFT_Load_Glyph(face,index,flags[j/7])); dump_slot(face);
            if(face->glyph->format==FT_GLYPH_FORMAT_OUTLINE)
            {
                pFT_Outline_Transform(&face->glyph->outline,&matrix);
                pFT_Outline_Translate(&face->glyph->outline,32,0);
                number(pFT_Outline_Embolden(&face->glyph->outline,16));
                number(pFT_Render_Glyph(face->glyph,j%2 ? FT_RENDER_MODE_NORMAL : FT_RENDER_MODE_MONO));
                dump_slot(face);
            }
        }
    }
    number(pFT_Load_Glyph(face,0xffffffff,0));
    number(pFT_Done_Face(face)); HeapFree(GetProcessHeap(),0,data);
}
static void test_gdi(void)
{
    HDC dc=CreateCompatibleDC(NULL);
    const WCHAR *faces[]={L"Tahoma",L"ADV Japanese Fixture"};
    unsigned sizes[]={9,16,31};
    unsigned codes[]={32,65,66,0x3042,0x4e00};
    unsigned formats[]={GGO_BITMAP,GGO_GRAY8_BITMAP,GGO_NATIVE,GGO_GRAY8_BITMAP|GGO_UNHINTED};
    unsigned f,s,q,g,format;
    if(!AddFontResourceExW(L"C:\\freetype.ttf",FR_PRIVATE,NULL)) ExitProcess(7);
    for(f=0;f<2;f++) for(s=0;s<3;s++) for(q=0;q<2;q++)
    {
        HFONT font=CreateFontW(-sizes[s],0,q*170,q*170,q?FW_BOLD:FW_NORMAL,q,0,0,DEFAULT_CHARSET,0,0,q?ANTIALIASED_QUALITY:NONANTIALIASED_QUALITY,0,faces[f]);
        HFONT old=SelectObject(dc,font);
        TEXTMETRICW tm={0};
        number(GetTextMetricsW(dc,&tm)); record(&tm,sizeof(tm));
        for(g=0;g<5;g++)
        {
            ABC abc={0};
            number(GetCharABCWidthsW(dc,codes[g],codes[g],&abc)); record(&abc,sizeof(abc));
            for(format=0;format<4;format++)
            {
                GLYPHMETRICS metrics={0};
                MAT2 mat={{0,1},{0,0},{0,0},{0,1}};
                DWORD length=GetGlyphOutlineW(dc,codes[g],formats[format],&metrics,0,NULL,&mat);
                number(length); record(&metrics,sizeof(metrics));
                if(length!=GDI_ERROR && length)
                {
                    BYTE *buffer=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,length);
                    number(GetGlyphOutlineW(dc,codes[g],formats[format],&metrics,length,buffer,&mat));
                    record(buffer,length); HeapFree(GetProcessHeap(),0,buffer);
                }
            }
        }
        SelectObject(dc,old); DeleteObject(font);
    }
    DeleteDC(dc);
}
int main(void)
{
    typedef DWORD (WINAPI *OPEN)(const char *,DWORD);
    typedef void *(WINAPI *SYM)(DWORD,const char *);
    HMODULE module=LoadLibraryA("freetype-test.dll");
    OPEN open=(OPEN)GetProcAddress(module,"OpenFreeType");
    SYM symbol=(SYM)GetProcAddress(module,"FreeTypeSymbol");
    DWORD handle;
    FT_Library library;
    int major,minor,patch;
    unsigned interpreter=40;
    if(!open || !symbol) return 1;
    handle=open("libfreetype.so.6",2);
#define LOAD(f) p##f=symbol(handle,#f); if(!p##f) {printf("missing %s\n",#f); return 2;}
    FUNCTIONS(LOAD)
    output=CreateFileA("C:\\freetype.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    number(pFT_Init_FreeType(&library));
    pFT_Library_Version(library,&major,&minor,&patch);
    printf("FreeType=%d.%d.%d\n",major,minor,patch);
    number(major);number(minor);number(patch);
    number(pFT_Get_TrueType_Engine_Type(library));
    number(pFT_Library_SetLcdFilter(library,FT_LCD_FILTER_DEFAULT));
    number(pFT_Property_Set(library,"truetype","interpreter-version",&interpreter));
    number(pFT_Done_Face(NULL));
    number(pFT_Get_Char_Index(NULL,65));
    number(pFT_Set_Charmap(NULL,NULL)); number(pFT_Set_Pixel_Sizes(NULL,0,16));
    number(pFT_Load_Glyph(NULL,0,0)); number(pFT_Render_Glyph(NULL,FT_RENDER_MODE_NORMAL));
    number(pFT_Get_Sfnt_Name(NULL,0,NULL)); number(pFT_Get_Sfnt_Name_Count(NULL));
    number(pFT_Get_WinFNT_Header(NULL,NULL));
    number(pFT_Library_SetLcdFilter(NULL,FT_LCD_FILTER_NONE));
    number(pFT_Property_Set(library,"missing","missing",&interpreter));
    number(pFT_Property_Set(library,"truetype","missing",&interpreter));
    test_math(); test_outlines(library);
    {
        typedef const char *(WINAPI *PATH)(void);
        PATH path=(PATH)GetProcAddress(module,"FontPath");
        unix_font=path();
        test_face(library,"C:\\freetype.ttf",0);
        test_face(library,"C:\\freetype.ttf",1);
    }
    test_gdi();
    CloseHandle(output);
    printf("records=%u\n",records);
    return 0;
}
