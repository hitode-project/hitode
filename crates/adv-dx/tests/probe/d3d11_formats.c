/* Supplemental Windows hardware and WARP format rows.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -Wextra -Werror -o d3d11_formats.exe d3d11_formats.c -ld3d11 -ld3dcompiler
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <d3dcompiler.h>
#include <stdio.h>

static unsigned bytes_per_row(DXGI_FORMAT format)
{
    switch (format) {
        case DXGI_FORMAT_R32G32B32_TYPELESS: return 48;
        case DXGI_FORMAT_R32G32B32A32_TYPELESS: return 64;
        case DXGI_FORMAT_R16G16B16A16_TYPELESS:
        case DXGI_FORMAT_R32G32_TYPELESS:
        case DXGI_FORMAT_R32G8X24_TYPELESS: return 32;
        case DXGI_FORMAT_R8G8_B8G8_UNORM:
        case DXGI_FORMAT_G8R8_G8B8_UNORM:
        case DXGI_FORMAT_R8G8_TYPELESS:
        case DXGI_FORMAT_R16_TYPELESS:
        case DXGI_FORMAT_B4G4R4A4_UNORM:
        case DXGI_FORMAT_BC1_TYPELESS:
        case DXGI_FORMAT_BC4_TYPELESS: return 8;
        case DXGI_FORMAT_BC5_TYPELESS:
        case DXGI_FORMAT_BC6H_TYPELESS: return 16;
        case DXGI_FORMAT_R8_TYPELESS:
        case DXGI_FORMAT_A8_UNORM: return 4;
        default: return 16;
    }
}

static void support_row(ID3D11Device *device, DXGI_FORMAT source)
{
    UINT flags = 0, bits[3] = {0};
    const UINT samples[] = {2, 4, 8};
    HRESULT support = ID3D11Device_CheckFormatSupport(device, source, &flags);
    D3D11_FEATURE_DATA_FORMAT_SUPPORT2 support2 = {source, 0};
    HRESULT extra = ID3D11Device_CheckFeatureSupport(device, D3D11_FEATURE_FORMAT_SUPPORT2,
                                                     &support2, sizeof(support2));
    for (unsigned i = 0; i < 3; ++i)
        ID3D11Device_CheckMultisampleQualityLevels(device, source, samples[i], &bits[i]);
    printf("support\t%u\t%08lx\t%08x\t%08lx\t%08x\t%u\t%u\t%u\n",
           source, (unsigned long)support, flags, (unsigned long)extra,
           support2.OutFormatSupport2, bits[0], bits[1], bits[2]);
}

static void exercise(ID3D11Device *device, ID3D11DeviceContext *ctx, DXGI_FORMAT source,
                     const DXGI_FORMAT *views, unsigned count, BOOL depth)
{
    support_row(device, source);
    for (unsigned i = 0; i < count; ++i)
        if (views[i] != source) support_row(device, views[i]);

    const UINT row = bytes_per_row(source);
    BYTE bytes[4 * 64] = {0};
    if (source == DXGI_FORMAT_R32G8X24_TYPELESS) {
        for (unsigned n = 0; n < 16; ++n) {
            static const float normalized[] = {0.0f, 0.125f, 0.5f, 1.0f};
            memcpy(bytes + n * 8, &normalized[n % 4], sizeof(float));
            bytes[n * 8 + 4] = (BYTE)(n + 7);
        }
    } else {
        for (unsigned n = 0; n < sizeof(bytes); ++n) bytes[n] = (BYTE)(n * 9 + 31);
    }
    D3D11_TEXTURE2D_DESC desc = {
        .Width = 4, .Height = 4, .MipLevels = 1, .ArraySize = 1, .Format = source,
        .SampleDesc = {1, 0}, .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_SHADER_RESOURCE | (depth ? D3D11_BIND_DEPTH_STENCIL : 0),
    };
    D3D11_SUBRESOURCE_DATA data = {bytes, row, 4 * row};
    ID3D11Texture2D *texture = NULL, *staging = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(device, &desc, &data, &texture);
    printf("texture\t%u\t%08lx\n", source, (unsigned long)hr);
    if (FAILED(hr)) return;
    for (unsigned i = 0; i < count; ++i) {
        D3D11_SHADER_RESOURCE_VIEW_DESC srv = {
            .Format = views[i], .ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D,
            .Texture2D = {0, 1},
        };
        ID3D11ShaderResourceView *view = NULL;
        hr = ID3D11Device_CreateShaderResourceView(device, (ID3D11Resource *)texture, &srv, &view);
        printf("typed-srv\t%u\t%u\t%08lx\n", source, views[i], (unsigned long)hr);
        if (view) ID3D11ShaderResourceView_Release(view);
    }
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &staging);
    printf("readback\t%u\t%08lx", source, (unsigned long)hr);
    if (SUCCEEDED(hr)) {
        D3D11_MAPPED_SUBRESOURCE mapped = {0};
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)texture);
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        printf("\t%08lx", (unsigned long)hr);
        if (SUCCEEDED(hr)) {
            printf("\t%u\t", mapped.RowPitch);
            for (unsigned i = 0; i < row; ++i) printf("%02x", ((BYTE *)mapped.pData)[i]);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        }
        ID3D11Texture2D_Release(staging);
    }
    puts("");
    ID3D11Texture2D_Release(texture);
}

static void alpha_uav(ID3D11Device *device, ID3D11DeviceContext *ctx)
{
    static const char hlsl[] =
        "RWTexture2D<float4> target : register(u0);"
        "[numthreads(1,1,1)] void main() { target[uint2(1,0)] = float4(1,.5,.25,0); }";
    D3D11_TEXTURE2D_DESC desc = {
        .Width = 2, .Height = 1, .MipLevels = 1, .ArraySize = 1, .Format = DXGI_FORMAT_A8_UNORM,
        .SampleDesc = {1, 0}, .BindFlags = D3D11_BIND_UNORDERED_ACCESS,
    };
    ID3D11Texture2D *image = NULL, *staging = NULL;
    ID3D11UnorderedAccessView *uav = NULL;
    ID3D11ComputeShader *shader = NULL;
    ID3DBlob *code = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &image);
    printf("a8-uav-texture\t%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) goto done;
    hr = ID3D11Device_CreateUnorderedAccessView(device, (ID3D11Resource *)image, NULL, &uav);
    printf("a8-uav-view\t%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) goto done;
    hr = D3DCompile(hlsl, sizeof(hlsl)-1, NULL, NULL, NULL, "main", "cs_5_0", 0, 0, &code, NULL);
    if (SUCCEEDED(hr))
        hr = ID3D11Device_CreateComputeShader(device, ID3D10Blob_GetBufferPointer(code),
                                              ID3D10Blob_GetBufferSize(code), NULL, &shader);
    printf("a8-uav-shader\t%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) goto done;
    const float clear[4] = {0.125f, 0.25f, 0.5f, 1.0f};
    ID3D11DeviceContext_ClearUnorderedAccessViewFloat(ctx, uav, clear);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &uav, NULL);
    ID3D11DeviceContext_CSSetShader(ctx, shader, NULL, 0);
    ID3D11DeviceContext_Dispatch(ctx, 1, 1, 1);
    ID3D11UnorderedAccessView *none = NULL;
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, 1, &none, NULL);
    ID3D11DeviceContext_CSSetShader(ctx, NULL, NULL, 0);
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &staging);
    if (SUCCEEDED(hr)) {
        D3D11_MAPPED_SUBRESOURCE map = {0};
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)image);
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &map);
        if (SUCCEEDED(hr)) {
            const BYTE *texels = (const BYTE *)map.pData;
            printf("a8-uav-bytes\t%02x\t%02x\n", texels[0], texels[1]);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        }
    }
    printf("a8-uav-readback\t%08lx\n", (unsigned long)hr);
done:
    if (staging) ID3D11Texture2D_Release(staging);
    if (shader) ID3D11ComputeShader_Release(shader);
    if (code) ID3D10Blob_Release(code);
    if (uav) ID3D11UnorderedAccessView_Release(uav);
    if (image) ID3D11Texture2D_Release(image);
}

static void arrays(ID3D11Device *device)
{
    D3D11_TEXTURE1D_DESC d1 = {
        .Width = 4, .MipLevels = 2, .ArraySize = 2, .Format = DXGI_FORMAT_R8G8B8A8_UNORM,
        .BindFlags = D3D11_BIND_SHADER_RESOURCE,
    };
    D3D11_SHADER_RESOURCE_VIEW_DESC srv = {
        .Format = d1.Format, .ViewDimension = D3D11_SRV_DIMENSION_TEXTURE1DARRAY,
        .Texture1DArray = {0, 2, 1, 1},
    };
    ID3D11Texture1D *narrow = NULL;
    HRESULT hr = ID3D11Device_CreateTexture1D(device, &d1, NULL, &narrow);
    printf("texture1d-array\t%08lx", (unsigned long)hr);
    if (SUCCEEDED(hr)) {
        ID3D11ShaderResourceView *view = NULL;
        hr = ID3D11Device_CreateShaderResourceView(device, (ID3D11Resource *)narrow, &srv, &view);
        printf("\t%08lx", (unsigned long)hr);
        if (view) ID3D11ShaderResourceView_Release(view);
        ID3D11Texture1D_Release(narrow);
    }
    puts("");
    D3D11_TEXTURE2D_DESC d2 = {
        .Width = 4, .Height = 1, .MipLevels = 1, .ArraySize = 2, .Format = d1.Format,
        .SampleDesc = {4, 0}, .BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET,
    };
    srv.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2DMSARRAY;
    srv.Texture2DMSArray = (D3D11_TEX2DMS_ARRAY_SRV){0, 2};
    ID3D11Texture2D *ms = NULL;
    hr = ID3D11Device_CreateTexture2D(device, &d2, NULL, &ms);
    printf("texture2dms-array\t%08lx", (unsigned long)hr);
    if (SUCCEEDED(hr)) {
        ID3D11ShaderResourceView *view = NULL;
        hr = ID3D11Device_CreateShaderResourceView(device, (ID3D11Resource *)ms, &srv, &view);
        printf("\t%08lx", (unsigned long)hr);
        if (view) ID3D11ShaderResourceView_Release(view);
        ID3D11Texture2D_Release(ms);
    }
    puts("");
}

int main(void)
{
    static const D3D_FEATURE_LEVEL levels[] = {
        D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    };
    static const struct {
        DXGI_FORMAT source;
        DXGI_FORMAT views[5];
        unsigned count;
        BOOL depth;
    } cases[] = {
        {DXGI_FORMAT_R32G32B32A32_TYPELESS, {2, 3, 4}, 3, FALSE},
        {DXGI_FORMAT_R32G32B32_TYPELESS, {6, 7, 8}, 3, FALSE},
        {DXGI_FORMAT_R16G16B16A16_TYPELESS, {10, 11, 12, 13, 14}, 5, FALSE},
        {DXGI_FORMAT_R32G32_TYPELESS, {16, 17, 18}, 3, FALSE},
        {DXGI_FORMAT_R32G8X24_TYPELESS, {21, 22}, 2, TRUE},
        {DXGI_FORMAT_R10G10B10A2_TYPELESS, {24, 25}, 2, FALSE},
        {DXGI_FORMAT_R8G8B8A8_TYPELESS, {28, 29, 30, 31, 32}, 5, FALSE},
        {DXGI_FORMAT_R16G16_TYPELESS, {34, 35, 36, 37, 38}, 5, FALSE},
        {DXGI_FORMAT_R32_TYPELESS, {41, 42, 43}, 3, FALSE},
        {DXGI_FORMAT_R32_TYPELESS, {41, 42, 43}, 3, TRUE},
        {DXGI_FORMAT_R24G8_TYPELESS, {46, 47}, 2, TRUE},
        {DXGI_FORMAT_R8G8_TYPELESS, {49, 50, 51, 52}, 4, FALSE},
        {DXGI_FORMAT_R16_TYPELESS, {54, 56, 57, 58, 59}, 5, FALSE},
        {DXGI_FORMAT_R16_TYPELESS, {54, 56, 57, 58, 59}, 5, TRUE},
        {DXGI_FORMAT_R8_TYPELESS, {61, 62, 63, 64}, 4, FALSE},
        {DXGI_FORMAT_A8_UNORM, {65}, 1, FALSE},
        {DXGI_FORMAT_R8G8_B8G8_UNORM, {68}, 1, FALSE},
        {DXGI_FORMAT_G8R8_G8B8_UNORM, {69}, 1, FALSE},
        {DXGI_FORMAT_BC1_TYPELESS, {71, 72}, 2, FALSE},
        {DXGI_FORMAT_BC4_TYPELESS, {80, 81}, 2, FALSE},
        {DXGI_FORMAT_BC5_TYPELESS, {83, 84}, 2, FALSE},
        {DXGI_FORMAT_B8G8R8A8_TYPELESS, {87, 91}, 2, FALSE},
        {DXGI_FORMAT_B8G8R8X8_TYPELESS, {88, 93}, 2, FALSE},
        {DXGI_FORMAT_BC6H_TYPELESS, {95, 96}, 2, FALSE},
        {DXGI_FORMAT_BC7_TYPELESS, {98, 99}, 2, FALSE},
        {DXGI_FORMAT_B4G4R4A4_UNORM, {115}, 1, FALSE},
    };
    for (unsigned warp = 0; warp != 2; ++warp)
    for (unsigned level = 0; level < sizeof(levels)/sizeof(*levels); ++level) {
        ID3D11Device *device = NULL;
        ID3D11DeviceContext *ctx = NULL;
        HRESULT hr = D3D11CreateDevice(NULL, warp ? D3D_DRIVER_TYPE_WARP : D3D_DRIVER_TYPE_HARDWARE,
                                       NULL, 0, &levels[level], 1, D3D11_SDK_VERSION, &device, NULL, &ctx);
        printf("device\t%s\t%04x\t%08lx\n", warp ? "warp" : "hw", levels[level], (unsigned long)hr);
        if (FAILED(hr)) continue;
        for (unsigned i = 0; i < sizeof(cases)/sizeof(*cases); ++i)
            exercise(device, ctx, cases[i].source, cases[i].views, cases[i].count, cases[i].depth);
        arrays(device);
        if (levels[level] == D3D_FEATURE_LEVEL_11_0) alpha_uav(device, ctx);
        ID3D11DeviceContext_Release(ctx);
        ID3D11Device_Release(device);
    }
}
