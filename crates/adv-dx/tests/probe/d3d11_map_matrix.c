/* Windows hardware and WARP oracle.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o d3d11_map_matrix.exe d3d11_map_matrix.c -ld3d11
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>

static void test(ID3D11Device *device, ID3D11DeviceContext *context, const char *name,
                 D3D11_USAGE usage, UINT bind, UINT cpu, BOOL texture)
{
    ID3D11Resource *res = NULL;
    HRESULT hr;
    if (texture) {
        D3D11_TEXTURE2D_DESC desc = {16, 16, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0}, usage, bind, cpu, 0};
        hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, (ID3D11Texture2D **)&res);
    } else {
        D3D11_BUFFER_DESC desc = {64, usage, bind, cpu, 0, 0};
        hr = ID3D11Device_CreateBuffer(device, &desc, NULL, (ID3D11Buffer **)&res);
    }
    if (!res) {
        printf("resource\t%s\t%08lx\n", name, (unsigned long)hr);
        return;
    }
    for (unsigned mode = 1; mode <= 5; ++mode)
        for (unsigned f = 0; f < 3; ++f) {
            UINT flags = f == 0 ? 0 : f == 1 ? D3D11_MAP_FLAG_DO_NOT_WAIT : 1;
            D3D11_MAPPED_SUBRESOURCE mapped = {0};
            hr = ID3D11DeviceContext_Map(context, res, 0, mode, flags, &mapped);
            printf("map\t%s\t%u\t%u\t%08lx\n", name, mode, flags, (unsigned long)hr);
            if (SUCCEEDED(hr)) ID3D11DeviceContext_Unmap(context, res, 0);
        }
    ID3D11Resource_Release(res);
}

int main(void)
{
    const D3D_FEATURE_LEVEL levels[] = {0xb000, 0xa000, 0x9100};
    for (unsigned warp = 0; warp < 2; ++warp)
    for (unsigned l = 0; l < 3; ++l) {
        ID3D11Device *device = NULL;
        ID3D11DeviceContext *context = NULL;
        HRESULT hr = D3D11CreateDevice(NULL, warp ? D3D_DRIVER_TYPE_WARP : D3D_DRIVER_TYPE_HARDWARE,
                                        NULL, 0, &levels[l], 1, 7, &device, NULL, &context);
        printf("device\t%s\t%04x\t%08lx\n", warp ? "warp" : "hw", (unsigned)levels[l], (unsigned long)hr);
        if (FAILED(hr)) continue;
        test(device, context, "dynamic-vb", D3D11_USAGE_DYNAMIC, D3D11_BIND_VERTEX_BUFFER, D3D11_CPU_ACCESS_WRITE, 0);
        test(device, context, "dynamic-cb", D3D11_USAGE_DYNAMIC, D3D11_BIND_CONSTANT_BUFFER, D3D11_CPU_ACCESS_WRITE, 0);
        test(device, context, "dynamic-srv", D3D11_USAGE_DYNAMIC, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_WRITE, 0);
        test(device, context, "dynamic-tex", D3D11_USAGE_DYNAMIC, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_WRITE, 1);
        test(device, context, "staging-read", D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, 0);
        test(device, context, "staging-write", D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_WRITE, 0);
        test(device, context, "staging-both", D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_WRITE | D3D11_CPU_ACCESS_READ, 1);
        test(device, context, "default-vb", D3D11_USAGE_DEFAULT, D3D11_BIND_VERTEX_BUFFER, 0, 0);
        ID3D11DeviceContext_Release(context);
        ID3D11Device_Release(device);
    }
}
