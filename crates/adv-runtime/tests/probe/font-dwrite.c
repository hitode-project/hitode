/* Build: i686-w64-mingw32-gcc -O2 -Wall font-dwrite.c -o font-dwrite.exe -ldwrite -ld2d1 -lole32 -luuid -lgdi32 */
/* CFF outline and glyph-run analysis through Wine's DirectWrite PE frontend. */
#define COBJMACROS
#include <windows.h>
#include <initguid.h>
#include <d2d1.h>
#include <dwrite.h>
#include <stdio.h>
static UINT points,figures;
static HRESULT WINAPI query(IUnknown *self,REFIID iid,void **out){(void)iid;*out=self;return S_OK;}
static ULONG WINAPI ref(IUnknown *self){(void)self;return 2;}
static void WINAPI fill(ID2D1SimplifiedGeometrySink *self,D2D1_FILL_MODE mode){(void)self;(void)mode;}
static void WINAPI flags(ID2D1SimplifiedGeometrySink *self,D2D1_PATH_SEGMENT mode){(void)self;(void)mode;}
static void WINAPI begin(ID2D1SimplifiedGeometrySink *self,D2D1_POINT_2F point,D2D1_FIGURE_BEGIN mode){(void)self;(void)point;(void)mode;figures++;}
static void WINAPI lines(ID2D1SimplifiedGeometrySink *self,const D2D1_POINT_2F *p,UINT n){(void)self;(void)p;points+=n;}
static void WINAPI beziers(ID2D1SimplifiedGeometrySink *self,const D2D1_BEZIER_SEGMENT *p,UINT n){(void)self;(void)p;points+=n*3;}
static void WINAPI end(ID2D1SimplifiedGeometrySink *self,D2D1_FIGURE_END mode){(void)self;(void)mode;}
static HRESULT WINAPI close_sink(ID2D1SimplifiedGeometrySink *self){(void)self;return S_OK;}
static const ID2D1SimplifiedGeometrySinkVtbl vtbl={{query,ref,ref},fill,flags,begin,lines,beziers,end,close_sink};
int main(void){
 IDWriteFactory *factory=0;IDWriteFontFile *file=0;IDWriteFontFace *face=0;IDWriteGlyphRunAnalysis *analysis=0;
 WCHAR path[MAX_PATH];GetFullPathNameW(L"cff.otf",MAX_PATH,path,0);
 if(FAILED(DWriteCreateFactory(DWRITE_FACTORY_TYPE_ISOLATED,&IID_IDWriteFactory,(IUnknown**)&factory)))return 1;
 if(FAILED(IDWriteFactory_CreateFontFileReference(factory,path,0,&file)))return 2;
 BOOL supported=FALSE;DWRITE_FONT_FILE_TYPE ft;DWRITE_FONT_FACE_TYPE type;UINT32 count=0;
 if(FAILED(IDWriteFontFile_Analyze(file,&supported,&ft,&type,&count))||!supported)return 3;
 if(FAILED(IDWriteFactory_CreateFontFace(factory,type,1,&file,0,DWRITE_FONT_SIMULATIONS_NONE,&face)))return 4;
 UINT32 character=0x3042;UINT16 glyph=0;IDWriteFontFace_GetGlyphIndices(face,&character,1,&glyph);if(!glyph)return 5;
 ID2D1SimplifiedGeometrySink sink={&vtbl};
 HRESULT hr=IDWriteFontFace_GetGlyphRunOutline(face,24,&glyph,0,0,1,FALSE,FALSE,&sink);
 if(FAILED(hr)||!points||!figures)return 6;
 FLOAT advance=24;DWRITE_GLYPH_RUN run={face,24,1,&glyph,&advance,0,FALSE,0};
 if(FAILED(IDWriteFactory_CreateGlyphRunAnalysis(factory,&run,1.0f,0,DWRITE_RENDERING_MODE_ALIASED,DWRITE_MEASURING_MODE_NATURAL,0,0,&analysis)))return 7;
 RECT bounds;if(FAILED(IDWriteGlyphRunAnalysis_GetAlphaTextureBounds(analysis,DWRITE_TEXTURE_ALIASED_1x1,&bounds)))return 8;
 UINT size=(bounds.right-bounds.left)*(bounds.bottom-bounds.top);if(!size)return 9;BYTE *bitmap=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,size);
 if(FAILED(IDWriteGlyphRunAnalysis_CreateAlphaTexture(analysis,DWRITE_TEXTURE_ALIASED_1x1,&bounds,bitmap,size)))return 10;
 UINT ink=0;for(UINT i=0;i<size;i++)ink+=bitmap[i]!=0;if(!ink)return 11;
 FILE *out=fopen("font-dwrite.bin","wb");if(!out)return 12;fwrite(&figures,4,1,out);fwrite(&points,4,1,out);fwrite(&bounds,sizeof(bounds),1,out);fwrite(&size,4,1,out);fwrite(bitmap,1,size,out);fclose(out);
 HeapFree(GetProcessHeap(),0,bitmap);IDWriteGlyphRunAnalysis_Release(analysis);IDWriteFontFace_Release(face);IDWriteFontFile_Release(file);IDWriteFactory_Release(factory);return 0;
}
