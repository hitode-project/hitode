/* Authored text record oracle (WINE-UNIX-8) for Direct3D 11: GDI draws "Hi"
 * into a 64 x 32 DIB, UpdateSubresource puts the DIB's bits in a texture,
 * CopySubresourceRegion puts the texture at (8, 4) on a 128 x 64 back
 * buffer, the swap chain presents, and the back buffer is read back through
 * a staging texture. Exits with 0 when every call succeeds.
 * Build: i686-w64-mingw32-gcc -O2 -Wall text-records-d3d11.c -o text-records-d3d11.exe -ld3d11 -ldxgi -luuid -lgdi32 -luser32 */
#define COBJMACROS
#include <windows.h>
#include <initguid.h>
#include <d3d11.h>

static D3D11_TEXTURE2D_DESC texture(UINT width, UINT height, D3D11_USAGE usage, UINT bind,
                                    UINT access)
{
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = width;
    desc.Height = height;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.SampleDesc.Count = 1;
    desc.Usage = usage;
    desc.BindFlags = bind;
    desc.CPUAccessFlags = access;
    return desc;
}

int main(void)
{
    HWND window = CreateWindowExW(0, L"static", L"", WS_POPUP | WS_VISIBLE, 0, 0, 128, 64, NULL,
                                  NULL, NULL, NULL);
    BITMAPINFO info = {0};
    info.bmiHeader.biSize = sizeof info.bmiHeader;
    info.bmiHeader.biWidth = 64;
    info.bmiHeader.biHeight = -32;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    void *bits;
    HDC dc = CreateCompatibleDC(0);
    HBITMAP bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &bits, 0, 0);
    if (!window || !dc || !bitmap) return 1;
    SelectObject(dc, bitmap);
    SetTextColor(dc, RGB(255, 255, 255));
    SetBkMode(dc, TRANSPARENT);
    if (!TextOutW(dc, 4, 4, L"Hi", 2)) return 1;
    GdiFlush();

    DXGI_SWAP_CHAIN_DESC chain = {0};
    chain.BufferDesc.Width = 128;
    chain.BufferDesc.Height = 64;
    chain.BufferDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    chain.SampleDesc.Count = 1;
    chain.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    chain.BufferCount = 1;
    chain.OutputWindow = window;
    chain.Windowed = TRUE;
    chain.SwapEffect = DXGI_SWAP_EFFECT_DISCARD;
    IDXGISwapChain *swap;
    ID3D11Device *device;
    ID3D11DeviceContext *context;
    if (FAILED(D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, NULL, 0,
                                             D3D11_SDK_VERSION, &chain, &swap, &device, NULL,
                                             &context)))
        return 2;

    D3D11_TEXTURE2D_DESC desc = texture(64, 32, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0);
    ID3D11Texture2D *text, *back, *staging;
    if (FAILED(ID3D11Device_CreateTexture2D(device, &desc, NULL, &text)) ||
        FAILED(IDXGISwapChain_GetBuffer(swap, 0, &IID_ID3D11Texture2D, (void **)&back)))
        return 3;
    ID3D11DeviceContext_UpdateSubresource(context, (ID3D11Resource *)text, 0, NULL, bits, 64 * 4,
                                          0);
    ID3D11DeviceContext_CopySubresourceRegion(context, (ID3D11Resource *)back, 0, 8, 4, 0,
                                              (ID3D11Resource *)text, 0, NULL);
    if (FAILED(IDXGISwapChain_Present(swap, 0, 0))) return 4;

    desc = texture(128, 64, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ);
    D3D11_MAPPED_SUBRESOURCE mapped;
    if (FAILED(ID3D11Device_CreateTexture2D(device, &desc, NULL, &staging))) return 5;
    ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)staging, (ID3D11Resource *)back);
    if (FAILED(ID3D11DeviceContext_Map(context, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0,
                                       &mapped)))
        return 6;
    ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)staging, 0);
    return 0;
}
