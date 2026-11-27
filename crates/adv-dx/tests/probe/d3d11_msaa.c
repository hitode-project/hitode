/* Pending Windows hardware and WARP rows: 8x format quality levels,
 * CreateTexture2D HRESULT for counts with zero quality levels, standard and
 * center creation, sample index and coverage, attribute evaluation,
 * per-sample depth, copy, MS SRV load and sample positions, and resolve.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o d3d11_msaa.exe d3d11_msaa.c -ld3d11 -ld3dcompiler
 * Run: d3d11_msaa.exe [warp]
 */
#define COBJMACROS
#include <d3d11.h>
#include <d3dcompiler.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char hlsl[] =
    "struct Vertex { float4 p : SV_Position; float2 xy : TEXCOORD0; float2 sample_xy : TEXCOORD1; };"
    "Vertex edge(uint id : SV_VertexID) {"
    "  float2 p[3] = {float2(0, 3), float2(0, -1), float2(4, -1)};"
    "  Vertex v; v.xy = (p[id] * float2(1, -1) + 1) * 0.5;"
    "  v.sample_xy = v.xy; v.p = float4(p[id], v.xy.x, 1); return v;"
    "}"
    "float4 shade(noperspective float2 xy : TEXCOORD0, sample noperspective float2 sample_xy : TEXCOORD1,"
    "             uint i : SV_SampleIndex, uint coverage : SV_Coverage,"
    "             out uint mask : SV_Coverage) : SV_Target {"
    "  mask = 0x77; return float4(i + 1, coverage, sample_xy.x, EvaluateAttributeAtSample(xy, i).y);"
    "}"
    "float4 strip(uint id : SV_VertexID) : SV_Position {"
    "  return float4(float2((id << 1) & 2, id & 2) * 2 - 1, 0.5, 1);"
    "}"
    "Texture2DMS<float4, 8> image : register(t0);"
    "float4 load(float4 p : SV_Position) : SV_Target { return image.Load(int2(0, 0), uint(p.x)); }"
    "float4 position(float4 p : SV_Position) : SV_Target {"
    "  return float4(image.GetSamplePosition(uint(p.x)), 0, 0);"
    "}";

static void check(HRESULT hr, const char *operation)
{
    if (FAILED(hr)) {
        printf("error\t%s\t%08lx\n", operation, (unsigned long)hr);
        exit(1);
    }
}
#define CHECK(call) check((call), #call)

static ID3DBlob *compile(const char *entry, const char *profile)
{
    ID3DBlob *code = NULL, *errors = NULL;
    HRESULT hr = D3DCompile(hlsl, sizeof(hlsl) - 1, NULL, NULL, NULL, entry, profile,
        0, 0, &code, &errors);
    if (errors) {
        fputs((const char *)ID3D10Blob_GetBufferPointer(errors), stderr);
        ID3D10Blob_Release(errors);
    }
    check(hr, entry);
    return code;
}

static ID3D11Texture2D *texture(ID3D11Device *device, D3D11_TEXTURE2D_DESC desc)
{
    ID3D11Texture2D *texture;
    CHECK(ID3D11Device_CreateTexture2D(device, &desc, NULL, &texture));
    return texture;
}

static void vertices(ID3D11Device *device, ID3D11DeviceContext *ctx, const char *entry)
{
    ID3DBlob *code = compile(entry, "vs_5_0");
    ID3D11VertexShader *vs;
    CHECK(ID3D11Device_CreateVertexShader(device, ID3D10Blob_GetBufferPointer(code),
        ID3D10Blob_GetBufferSize(code), NULL, &vs));
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11VertexShader_Release(vs);
    ID3D10Blob_Release(code);
}

static void pixels(ID3D11Device *device, ID3D11DeviceContext *ctx, const char *entry)
{
    ID3DBlob *code = compile(entry, "ps_5_0");
    ID3D11PixelShader *ps;
    CHECK(ID3D11Device_CreatePixelShader(device, ID3D10Blob_GetBufferPointer(code),
        ID3D10Blob_GetBufferSize(code), NULL, &ps));
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11PixelShader_Release(ps);
    ID3D10Blob_Release(code);
}

static void read_strip(ID3D11DeviceContext *ctx, ID3D11Texture2D *staging,
                       ID3D11Texture2D *source, const char *entry)
{
    D3D11_MAPPED_SUBRESOURCE mapped;
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)source);
    CHECK(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped));
    const float *values = mapped.pData;
    for (unsigned i = 0; i < 8; ++i)
        printf("%s\t%u\t%g\t%g\t%g\t%g\n", entry, i,
            values[4*i], values[4*i+1], values[4*i+2], values[4*i+3]);
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
}

static void unsupported_counts(ID3D11Device *device)
{
    const DXGI_FORMAT formats[] = {DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_B5G6R5_UNORM};
    const UINT counts[] = {2, 4, 8, 32};
    for (unsigned f = 0; f < sizeof(formats) / sizeof(*formats); ++f) {
        for (unsigned s = 0; s < sizeof(counts) / sizeof(*counts); ++s) {
            UINT quality;
            HRESULT hr = ID3D11Device_CheckMultisampleQualityLevels(device, formats[f], counts[s],
                &quality);
            if (hr != S_OK || quality != 0) continue;
            const D3D11_TEXTURE2D_DESC desc = {1, 1, 1, 1, formats[f],
                {counts[s], 0}, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0};
            ID3D11Texture2D *image = NULL;
            hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &image);
            printf("create-unsupported-count\t%u\t%u\t%08lx\n",
                (unsigned)formats[f], counts[s], (unsigned long)hr);
            if (image) ID3D11Texture2D_Release(image);
        }
    }
}

int main(int argc, char **argv)
{
    const D3D_DRIVER_TYPE driver = argc > 1 && !strcmp(argv[1], "warp")
        ? D3D_DRIVER_TYPE_WARP : D3D_DRIVER_TYPE_HARDWARE;
    const D3D_FEATURE_LEVEL level = D3D_FEATURE_LEVEL_11_0;
    ID3D11Device *device;
    ID3D11DeviceContext *ctx;
    CHECK(D3D11CreateDevice(NULL, driver, NULL, 0, &level, 1, D3D11_SDK_VERSION,
        &device, NULL, &ctx));
    unsupported_counts(device);
    for (unsigned format = 1; format <= 115; ++format) {
        UINT quality = 0;
        HRESULT hr = ID3D11Device_CheckMultisampleQualityLevels(device, (DXGI_FORMAT)format,
            8, &quality);
        printf("quality-8x\t%u\t%08lx\t%u\n", format, (unsigned long)hr, quality);
    }

    D3D11_TEXTURE2D_DESC desc = {1, 1, 1, 1, DXGI_FORMAT_R16G16B16A16_FLOAT,
        {8, D3D11_STANDARD_MULTISAMPLE_PATTERN}, D3D11_USAGE_DEFAULT,
        D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, 0};
    ID3D11Texture2D *msaa = texture(device, desc), *copy = texture(device, desc);
    ID3D11Texture2D *center = NULL;
    desc.SampleDesc.Quality = D3D11_CENTER_MULTISAMPLE_PATTERN;
    HRESULT hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &center);
    printf("create-center-8x\t%08lx\n", (unsigned long)hr);
    if (center) ID3D11Texture2D_Release(center);
    desc.SampleDesc.Quality = D3D11_STANDARD_MULTISAMPLE_PATTERN;
    desc.Format = DXGI_FORMAT_D32_FLOAT;
    desc.BindFlags = D3D11_BIND_DEPTH_STENCIL;
    ID3D11Texture2D *depth = texture(device, desc);

    desc.SampleDesc = (DXGI_SAMPLE_DESC){1, 0};
    desc.Format = DXGI_FORMAT_R16G16B16A16_FLOAT;
    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    ID3D11Texture2D *resolved = texture(device, desc);
    desc.BindFlags = 0;
    desc.Usage = D3D11_USAGE_STAGING;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ID3D11Texture2D *resolved_staging = texture(device, desc);

    desc.Width = 8;
    desc.Format = DXGI_FORMAT_R32G32B32A32_FLOAT;
    ID3D11Texture2D *staging = texture(device, desc);
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.CPUAccessFlags = 0;
    desc.BindFlags = D3D11_BIND_RENDER_TARGET;
    ID3D11Texture2D *strip = texture(device, desc);
    ID3D11RenderTargetView *rt, *strip_rt;
    ID3D11DepthStencilView *ds;
    ID3D11ShaderResourceView *srv, *unbound = NULL;
    CHECK(ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)msaa, NULL, &rt));
    CHECK(ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)strip, NULL, &strip_rt));
    CHECK(ID3D11Device_CreateDepthStencilView(device, (ID3D11Resource *)depth, NULL, &ds));
    CHECK(ID3D11Device_CreateShaderResourceView(device, (ID3D11Resource *)copy, NULL, &srv));

    D3D11_RASTERIZER_DESC raster = {D3D11_FILL_SOLID, D3D11_CULL_NONE, FALSE, 0, 0, 0,
        TRUE, FALSE, TRUE, FALSE};
    ID3D11RasterizerState *state;
    CHECK(ID3D11Device_CreateRasterizerState(device, &raster, &state));
    ID3D11DeviceContext_RSSetState(ctx, state);
    ID3D11RasterizerState_Release(state);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

    for (unsigned depth_test = 0; depth_test < 2; ++depth_test) {
        D3D11_VIEWPORT viewport = {0, 0, 1, 1, 0, 1};
        const float clear[4] = {0, 0, 0, 0};
        vertices(device, ctx, "edge");
        pixels(device, ctx, "shade");
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &viewport);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rt, depth_test ? ds : NULL);
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rt, clear);
        if (depth_test) ID3D11DeviceContext_ClearDepthStencilView(ctx, ds, D3D11_CLEAR_DEPTH, 0.75f, 0);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 0, NULL, NULL);
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)copy, (ID3D11Resource *)msaa);
        ID3D11DeviceContext_ResolveSubresource(ctx, (ID3D11Resource *)resolved, 0,
            (ID3D11Resource *)copy, 0, DXGI_FORMAT_R16G16B16A16_FLOAT);
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)resolved_staging,
            (ID3D11Resource *)resolved);
        D3D11_MAPPED_SUBRESOURCE mapped;
        CHECK(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)resolved_staging, 0,
            D3D11_MAP_READ, 0, &mapped));
        const unsigned short *value = mapped.pData;
        printf("resolve-8x\t%u\t%04x\t%04x\t%04x\t%04x\n",
            depth_test, value[0], value[1], value[2], value[3]);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)resolved_staging, 0);

        viewport.Width = 8;
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &viewport);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &strip_rt, NULL);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &srv);
        vertices(device, ctx, "strip");
        pixels(device, ctx, "load");
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        read_strip(ctx, staging, strip, depth_test ? "depth-8x-load" : "coverage-8x-load");
        if (!depth_test) {
            pixels(device, ctx, "position");
            ID3D11DeviceContext_Draw(ctx, 3, 0);
            read_strip(ctx, staging, strip, "position-8x");
        }
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &unbound);
    }
    ID3D11DeviceContext_ClearState(ctx);
    ID3D11ShaderResourceView_Release(srv);
    ID3D11DepthStencilView_Release(ds);
    ID3D11RenderTargetView_Release(strip_rt);
    ID3D11RenderTargetView_Release(rt);
    ID3D11Texture2D_Release(strip);
    ID3D11Texture2D_Release(staging);
    ID3D11Texture2D_Release(resolved_staging);
    ID3D11Texture2D_Release(resolved);
    ID3D11Texture2D_Release(depth);
    ID3D11Texture2D_Release(copy);
    ID3D11Texture2D_Release(msaa);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(device);
    return 0;
}
