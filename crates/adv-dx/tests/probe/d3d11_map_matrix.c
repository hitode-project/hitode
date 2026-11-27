/* Windows hardware and WARP oracle.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o d3d11_map_matrix.exe d3d11_map_matrix.c -ld3d11
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <string.h>

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

/* Supplemental rows pending hardware and WARP capture. */
static void test_buffer_copies(ID3D11Device *device, ID3D11DeviceContext *context, UINT bind)
{
    const UINT old[] = {1, 2, 3, 4}, fresh[] = {11, 22, 33, 44};
    const D3D11_SUBRESOURCE_DATA old_data = {old, 0, 0}, fresh_data = {fresh, 0, 0};
    D3D11_BUFFER_DESC desc = {sizeof(old), D3D11_USAGE_DYNAMIC, bind, D3D11_CPU_ACCESS_WRITE, 0, 0};
    ID3D11Buffer *dynamic = NULL, *source = NULL, *staging = NULL;
    ID3D11UnorderedAccessView *counter = NULL;
    if (FAILED(ID3D11Device_CreateBuffer(device, &desc, &old_data, &dynamic))) goto done;
    desc = (D3D11_BUFFER_DESC){sizeof(old), D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, 0, 0};
    if (FAILED(ID3D11Device_CreateBuffer(device, &desc, NULL, &staging))) goto done;
    desc = (D3D11_BUFFER_DESC){sizeof(old), D3D11_USAGE_DEFAULT, D3D11_BIND_UNORDERED_ACCESS,
                               0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, sizeof(UINT)};
    if (FAILED(ID3D11Device_CreateBuffer(device, &desc, &fresh_data, &source))) goto done;
    D3D11_UNORDERED_ACCESS_VIEW_DESC view = {
        .ViewDimension = D3D11_UAV_DIMENSION_BUFFER,
        .Buffer = {0, 4, D3D11_BUFFER_UAV_FLAG_COUNTER}
    };
    if (FAILED(ID3D11Device_CreateUnorderedAccessView(device, (ID3D11Resource *)source, &view, &counter))) goto done;
    for (unsigned operation = 0; operation < 3; ++operation) {
        D3D11_MAPPED_SUBRESOURCE mapped;
        HRESULT hr = ID3D11DeviceContext_Map(context, (ID3D11Resource *)dynamic, 0,
                                              D3D11_MAP_WRITE_DISCARD, 0, &mapped);
        if (FAILED(hr)) {
            printf("dynamic-buffer-discard\t%u\t%08lx\n", bind, (unsigned long)hr);
            continue;
        }
        memcpy(mapped.pData, old, sizeof(old));
        ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)dynamic, 0);
        const char *name;
        if (operation == 0) {
            name = "copy";
            ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)dynamic, (ID3D11Resource *)source);
        } else if (operation == 1) {
            name = "copy-region";
            const D3D11_BOX box = {4, 0, 0, 12, 1, 1};
            ID3D11DeviceContext_CopySubresourceRegion(context, (ID3D11Resource *)dynamic, 0, 4, 0, 0,
                                                      (ID3D11Resource *)source, 0, &box);
        } else {
            name = "copy-count";
            const UINT count = 7;
            ID3D11DeviceContext_CSSetUnorderedAccessViews(context, 0, 1, &counter, &count);
            ID3D11DeviceContext_CopyStructureCount(context, dynamic, 4, counter);
        }
        hr = ID3D11DeviceContext_Map(context, (ID3D11Resource *)dynamic, 0,
                                      D3D11_MAP_WRITE_NO_OVERWRITE, 0, &mapped);
        printf("dynamic-buffer\t%u\t%s\t%08lx", bind, name, (unsigned long)hr);
        if (SUCCEEDED(hr)) {
            ((UINT *)mapped.pData)[3] = 55;
            ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)dynamic, 0);
        }
        ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)staging, (ID3D11Resource *)dynamic);
        if (SUCCEEDED(ID3D11DeviceContext_Map(context, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped))) {
            const UINT *words = mapped.pData;
            printf("\t%u\t%u\t%u\t%u", words[0], words[1], words[2], words[3]);
            ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)staging, 0);
        }
        putchar('\n');
    }
done:
    if (counter) ID3D11UnorderedAccessView_Release(counter);
    if (source) ID3D11Buffer_Release(source);
    if (staging) ID3D11Buffer_Release(staging);
    if (dynamic) ID3D11Buffer_Release(dynamic);
}

static void test_texture_copies(ID3D11Device *device, ID3D11DeviceContext *context)
{
    const UINT old[] = {1, 2}, fresh[] = {11, 22};
    const D3D11_SUBRESOURCE_DATA old_data = {old, sizeof(old), 0};
    const D3D11_SUBRESOURCE_DATA fresh_data = {fresh, sizeof(fresh), 0};
    D3D11_TEXTURE2D_DESC desc = {2, 1, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0},
                                 D3D11_USAGE_DYNAMIC, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_WRITE, 0};
    ID3D11Texture2D *dynamic = NULL, *source = NULL, *staging = NULL, *msaa = NULL;
    ID3D11RenderTargetView *rtv = NULL;
    if (FAILED(ID3D11Device_CreateTexture2D(device, &desc, &old_data, &dynamic))) goto done;
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    if (FAILED(ID3D11Device_CreateTexture2D(device, &desc, NULL, &staging))) goto done;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.CPUAccessFlags = 0;
    if (FAILED(ID3D11Device_CreateTexture2D(device, &desc, &fresh_data, &source))) goto done;
    for (unsigned operation = 0; operation < 2; ++operation) {
        const char *name = operation == 0 ? "copy" : "resolve";
        if (operation == 0) {
            ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)dynamic, (ID3D11Resource *)source);
        } else {
            desc.SampleDesc.Count = 4;
            desc.BindFlags = D3D11_BIND_RENDER_TARGET;
            if (FAILED(ID3D11Device_CreateTexture2D(device, &desc, NULL, &msaa))) break;
            if (FAILED(ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)msaa, NULL, &rtv))) break;
            const FLOAT red[] = {1, 0, 0, 1};
            ID3D11DeviceContext_ClearRenderTargetView(context, rtv, red);
            ID3D11DeviceContext_ResolveSubresource(context, (ID3D11Resource *)dynamic, 0,
                                                    (ID3D11Resource *)msaa, 0, desc.Format);
        }
        D3D11_MAPPED_SUBRESOURCE mapped;
        HRESULT hr = ID3D11DeviceContext_Map(context, (ID3D11Resource *)dynamic, 0,
                                              D3D11_MAP_WRITE_NO_OVERWRITE, 0, &mapped);
        printf("dynamic-texture\t%s\t%08lx", name, (unsigned long)hr);
        if (SUCCEEDED(hr)) ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)dynamic, 0);
        ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)staging, (ID3D11Resource *)dynamic);
        if (SUCCEEDED(ID3D11DeviceContext_Map(context, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped))) {
            printf("\t%08x\t%08x", ((UINT *)mapped.pData)[0], ((UINT *)mapped.pData)[1]);
            ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)staging, 0);
        }
        putchar('\n');
    }
done:
    if (rtv) ID3D11RenderTargetView_Release(rtv);
    if (msaa) ID3D11Texture2D_Release(msaa);
    if (source) ID3D11Texture2D_Release(source);
    if (staging) ID3D11Texture2D_Release(staging);
    if (dynamic) ID3D11Texture2D_Release(dynamic);
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
        if (levels[l] == D3D_FEATURE_LEVEL_11_0) {
            const UINT binds[] = {D3D11_BIND_VERTEX_BUFFER, D3D11_BIND_INDEX_BUFFER,
                                  D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE};
            for (unsigned i = 0; i < sizeof(binds) / sizeof(*binds); ++i)
                test_buffer_copies(device, context, binds[i]);
            test_texture_copies(device, context);
        }
        ID3D11DeviceContext_Release(context);
        ID3D11Device_Release(device);
    }
}
