/* The DirectDraw probe: GDI draws a TrueType string into a DirectDraw
 * surface through the surface's DC, the surface is read back through Lock,
 * and then blitted to the primary surface. It exits with 0 only when the
 * text inked the surface, the background around it stayed as filled and
 * the blit succeeded.
 * Build: i686-w64-mingw32-gcc -O1 -o ddraw.exe ddraw.c -lddraw -ldxguid -lgdi32 -luser32 */
#define COBJMACROS
#include <windows.h>
#include <ddraw.h>
#include <stdio.h>

#define WIDTH 64
#define HEIGHT 32
#define WHITE 0xffffff
#define BLUE 0x0000ff

int main(void)
{
    IDirectDraw7 *draw;
    IDirectDrawSurface7 *surface, *primary;
    DDSURFACEDESC2 desc = {sizeof(desc)};
    DDBLTFX fx = {sizeof(fx)};
    HRESULT created, level, made, filled, got = E_FAIL, released = E_FAIL, locked, blitted = E_FAIL;
    HFONT font;
    HDC dc;
    int drawn = 0, inked = 0, background = 0, other = 0, bits = 0;
    long pitch = 0;
    HWND window;

    created = DirectDrawCreateEx(NULL, (void **)&draw, &IID_IDirectDraw7, NULL);
    if (FAILED(created)) return 2;
    window = CreateWindowExW(0, L"static", L"probe", WS_POPUP, 0, 0, WIDTH, HEIGHT,
                             NULL, NULL, NULL, NULL);
    level = IDirectDraw7_SetCooperativeLevel(draw, window, DDSCL_NORMAL);

    desc.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT;
    desc.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
    desc.dwWidth = WIDTH;
    desc.dwHeight = HEIGHT;
    desc.ddpfPixelFormat.dwSize = sizeof(desc.ddpfPixelFormat);
    desc.ddpfPixelFormat.dwFlags = DDPF_RGB;
    desc.ddpfPixelFormat.dwRGBBitCount = 32;
    desc.ddpfPixelFormat.dwRBitMask = 0xff0000;
    desc.ddpfPixelFormat.dwGBitMask = 0x00ff00;
    desc.ddpfPixelFormat.dwBBitMask = 0x0000ff;
    made = IDirectDraw7_CreateSurface(draw, &desc, &surface, NULL);
    if (FAILED(made)) return 3;

    fx.dwFillColor = BLUE;
    filled = IDirectDrawSurface7_Blt(surface, NULL, NULL, NULL, DDBLT_COLORFILL | DDBLT_WAIT, &fx);

    font = CreateFontW(-16, 0, 0, 0, FW_NORMAL, 0, 0, 0, ANSI_CHARSET, 0, 0,
                       NONANTIALIASED_QUALITY, 0, L"Tahoma");
    got = IDirectDrawSurface7_GetDC(surface, &dc);
    if (SUCCEEDED(got))
    {
        SelectObject(dc, font);
        SetTextColor(dc, RGB(255, 255, 255));
        SetBkMode(dc, TRANSPARENT);
        drawn = TextOutW(dc, 4, 4, L"Hi", 2);
        released = IDirectDrawSurface7_ReleaseDC(surface, dc);
    }

    memset(&desc, 0, sizeof(desc));
    desc.dwSize = sizeof(desc);
    locked = IDirectDrawSurface7_Lock(surface, NULL, &desc, DDLOCK_WAIT | DDLOCK_READONLY, NULL);
    if (SUCCEEDED(locked))
    {
        int x, y;
        pitch = desc.lPitch;
        bits = desc.ddpfPixelFormat.dwRGBBitCount;
        for (y = 0; y < HEIGHT; y++)
            for (x = 0; x < WIDTH; x++)
            {
                DWORD pixel = ((DWORD *)((BYTE *)desc.lpSurface + y * desc.lPitch))[x] & 0xffffff;
                if (pixel == WHITE) inked++;
                else if (pixel == BLUE) background++;
                else other++;
            }
        IDirectDrawSurface7_Unlock(surface, NULL);
    }

    memset(&desc, 0, sizeof(desc));
    desc.dwSize = sizeof(desc);
    desc.dwFlags = DDSD_CAPS;
    desc.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
    if (SUCCEEDED(IDirectDraw7_CreateSurface(draw, &desc, &primary, NULL)))
    {
        RECT rect = {0, 0, WIDTH, HEIGHT};
        blitted = IDirectDrawSurface7_Blt(primary, &rect, surface, NULL, DDBLT_WAIT, NULL);
        IDirectDrawSurface7_Release(primary);
    }

    IDirectDrawSurface7_Release(surface);
    IDirectDraw7_Release(draw);
    DestroyWindow(window);

    printf("level=%#lx filled=%#lx dc=%#lx drawn=%d released=%#lx bits=%d pitch=%ld "
           "inked=%d background=%d other=%d blitted=%#lx\n",
           level, filled, got, drawn, released, bits, pitch, inked, background, other, blitted);
    fflush(stdout);
    return !(level == DD_OK && filled == DD_OK && got == DD_OK && drawn && released == DD_OK &&
             inked > 0 && background + inked == WIDTH * HEIGHT && blitted == DD_OK);
}
