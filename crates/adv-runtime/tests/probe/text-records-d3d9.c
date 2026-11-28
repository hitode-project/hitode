/* Authored text record oracle (WINE-UNIX-8) for Direct3D 9: GDI draws "Hi"
 * into a managed texture through its surface's DC, a pre-transformed quad
 * draws the texture unscaled at (8, 4) on a 128 x 64 back buffer, the device
 * presents, and GetRenderTargetData reads the back buffer back into system
 * memory. Exits with 0 when every call succeeds.
 * Build: i686-w64-mingw32-gcc -O2 -Wall text-records-d3d9.c -o text-records-d3d9.exe -ld3d9 -lgdi32 -luser32 */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>

struct vertex
{
    float x, y, z, rhw, u, v;
};

int main(void)
{
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    if (!d3d) return 1;
    HWND window = CreateWindowExW(0, L"static", L"", WS_POPUP | WS_VISIBLE, 0, 0, 128, 64, NULL,
                                  NULL, NULL, NULL);
    D3DPRESENT_PARAMETERS params = {0};
    params.Windowed = TRUE;
    params.SwapEffect = D3DSWAPEFFECT_DISCARD;
    params.BackBufferWidth = 128;
    params.BackBufferHeight = 64;
    params.BackBufferFormat = D3DFMT_X8R8G8B8;
    params.hDeviceWindow = window;
    IDirect3DDevice9 *device;
    if (FAILED(IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, window,
                                       D3DCREATE_SOFTWARE_VERTEXPROCESSING, &params, &device)))
        return 2;

    IDirect3DTexture9 *texture;
    IDirect3DSurface9 *level;
    HDC dc;
    if (FAILED(IDirect3DDevice9_CreateTexture(device, 64, 32, 1, 0, D3DFMT_X8R8G8B8,
                                              D3DPOOL_MANAGED, &texture, NULL)) ||
        FAILED(IDirect3DTexture9_GetSurfaceLevel(texture, 0, &level)) ||
        FAILED(IDirect3DSurface9_GetDC(level, &dc)))
        return 3;
    RECT rect = {0, 0, 64, 32};
    FillRect(dc, &rect, GetStockObject(BLACK_BRUSH));
    SetTextColor(dc, RGB(255, 255, 255));
    SetBkMode(dc, TRANSPARENT);
    if (!TextOutW(dc, 4, 4, L"Hi", 2) || FAILED(IDirect3DSurface9_ReleaseDC(level, dc))) return 4;

    /* Texel centres at pixel centres: the quad maps the texture 1:1. */
    struct vertex quad[4] = {
        {8 - 0.5f, 4 - 0.5f, 0, 1, 0, 0},
        {72 - 0.5f, 4 - 0.5f, 0, 1, 1, 0},
        {8 - 0.5f, 36 - 0.5f, 0, 1, 0, 1},
        {72 - 0.5f, 36 - 0.5f, 0, 1, 1, 1},
    };
    if (FAILED(IDirect3DDevice9_Clear(device, 0, NULL, D3DCLEAR_TARGET, 0, 1, 0)) ||
        FAILED(IDirect3DDevice9_BeginScene(device)) ||
        FAILED(IDirect3DDevice9_SetFVF(device, D3DFVF_XYZRHW | D3DFVF_TEX1)) ||
        FAILED(IDirect3DDevice9_SetTexture(device, 0, (IDirect3DBaseTexture9 *)texture)) ||
        FAILED(IDirect3DDevice9_DrawPrimitiveUP(device, D3DPT_TRIANGLESTRIP, 2, quad,
                                                sizeof quad[0])) ||
        FAILED(IDirect3DDevice9_EndScene(device)) ||
        FAILED(IDirect3DDevice9_Present(device, NULL, NULL, NULL, NULL)))
        return 5;

    IDirect3DSurface9 *back, *copy;
    D3DLOCKED_RECT locked;
    if (FAILED(IDirect3DDevice9_GetBackBuffer(device, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back)) ||
        FAILED(IDirect3DDevice9_CreateOffscreenPlainSurface(device, 128, 64, D3DFMT_X8R8G8B8,
                                                            D3DPOOL_SYSTEMMEM, &copy, NULL)) ||
        FAILED(IDirect3DDevice9_GetRenderTargetData(device, back, copy)) ||
        FAILED(IDirect3DSurface9_LockRect(copy, &locked, NULL, D3DLOCK_READONLY)))
        return 6;
    IDirect3DSurface9_UnlockRect(copy);
    return 0;
}
