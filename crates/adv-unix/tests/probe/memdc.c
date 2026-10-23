/* The memory-DC probe: GDI draws into caller-owned pixels through
 * D3DKMTCreateDCFromMemory, the path a DirectDraw surface's GetDC takes.
 * It exits with 0 only when the pixels hold what GDI drew.
 * Build: i686-w64-mingw32-gcc -O1 -o memdc.exe memdc.c -lgdi32 */
#include <windows.h>
#include <stdio.h>

typedef struct
{
    const void *pMemory;
    UINT Format;
    UINT Width;
    UINT Height;
    UINT Pitch;
    HDC hDeviceDc;
    PALETTEENTRY *pColorTable;
    HDC hDc;
    HANDLE hBitmap;
} CREATEDCFROMMEMORY;

typedef struct
{
    HDC hDc;
    HANDLE hBitmap;
} DESTROYDCFROMMEMORY;

/* D3DDDIFMT_X8R8G8B8 */
#define FORMAT_X8R8G8B8 22
#define WIDTH 16
#define HEIGHT 8

int main(void)
{
    static DWORD pixels[HEIGHT][WIDTH];
    CREATEDCFROMMEMORY create = {0};
    DESTROYDCFROMMEMORY destroy;
    LONG (WINAPI *create_dc)(CREATEDCFROMMEMORY *);
    LONG (WINAPI *destroy_dc)(DESTROYDCFROMMEMORY *);
    HMODULE gdi = GetModuleHandleA("gdi32.dll");
    HBRUSH brush;
    RECT rect = {4, 2, 12, 6};
    LONG created, destroyed;
    int filled, inside, outside;

    create_dc = (void *)GetProcAddress(gdi, "D3DKMTCreateDCFromMemory");
    destroy_dc = (void *)GetProcAddress(gdi, "D3DKMTDestroyDCFromMemory");
    create.pMemory = pixels;
    create.Format = FORMAT_X8R8G8B8;
    create.Width = WIDTH;
    create.Height = HEIGHT;
    create.Pitch = WIDTH * 4;
    create.hDeviceDc = CreateCompatibleDC(NULL);
    created = create_dc(&create);
    brush = CreateSolidBrush(RGB(0x12, 0x34, 0x56));
    filled = FillRect(create.hDc, &rect, brush);
    GdiFlush();
    DeleteObject(brush);
    destroy.hDc = create.hDc;
    destroy.hBitmap = create.hBitmap;
    destroyed = destroy_dc(&destroy);
    DeleteDC(create.hDeviceDc);

    inside = pixels[2][4] == 0x123456 && pixels[5][11] == 0x123456;
    outside = pixels[1][4] == 0 && pixels[2][12] == 0 && pixels[6][11] == 0;
    printf("created=%ld filled=%d inside=%d outside=%d destroyed=%ld\n", created, filled, inside,
           outside, destroyed);
    return !created && filled && inside && outside && !destroyed ? 0 : 1;
}
