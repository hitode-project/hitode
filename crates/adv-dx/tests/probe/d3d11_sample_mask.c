/* Hardware and WARP sample mask rows.
 * Build: i686-w64-mingw32-gcc -O1 -o d3d11_sample_mask.exe d3d11_sample_mask.c -ld3d11 -ld3dcompiler_47 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <d3dcompiler.h>
#include <stdio.h>
#include <stdlib.h>

static void check(HRESULT hr)
{
    if (FAILED(hr)) {
        fprintf(stderr, "HRESULT %#lx\n", (unsigned long)hr);
        exit(1);
    }
}

static ID3DBlob *compile(const char *source, const char *profile)
{
    ID3DBlob *code;
    check(D3DCompile(source, strlen(source), NULL, NULL, NULL, "main", profile, 0, 0, &code, NULL));
    return code;
}

static void probe(D3D_DRIVER_TYPE driver, const char *name)
{
    static const char *const pixel_sources[] = {
        "cbuffer C { float4 c; }; float4 main() : SV_Target { return c; }",
        "float4 main(uint cov : SV_Coverage) : SV_Target { return float4(cov*4/255.0,1,0,1); }",
        "cbuffer C { float4 c; }; float4 main(out uint m : SV_Coverage) : SV_Target { m=asuint(c.z); return c; }",
        "cbuffer C { float4 c; }; float4 main(uint cov : SV_Coverage, out uint m : SV_Coverage) : SV_Target"
        " { m=asuint(c.z); return float4(cov*4/255.0,1,0,c.a); }"
    };
    static const struct test {
        UINT shader, mask, output, a2c;
        float alpha;
    } tests[] = {
        {0, ~0u, 0, 0, 1}, {0, 0, 0, 0, 1}, {0, 1, 0, 0, 1}, {0, 2, 0, 0, 1},
        {0, 4, 0, 0, 1}, {0, 8, 0, 0, 1}, {0, 0, 0, 1, 1}, {0, 1, 0, 1, 0},
        {0, 1, 0, 1, 1}, {0, 2, 0, 1, 1}, {2, ~0u, 1, 1, 0}, {2, 1, 0, 1, 0},
        {2, 1, 1, 1, 0}, {2, 2, 1, 1, 0}, {2, 3, 2, 1, 0},
        {1, ~0u, 0, 0, 1}, {1, 0, 0, 0, 1}, {1, 1, 0, 0, 1}, {1, 2, 0, 0, 1},
        {1, 4, 0, 0, 1}, {1, 5, 0, 0, 1}, {1, 3, 0, 0, 1}, {3, 3, 1, 0, 1},
    };
    ID3D11Device *dev;
    ID3D11DeviceContext *ctx;
    D3D_FEATURE_LEVEL level = D3D_FEATURE_LEVEL_11_0;
    check(D3D11CreateDevice(NULL, driver, NULL, 0, &level, 1, D3D11_SDK_VERSION, &dev, NULL, &ctx));
    ID3DBlob *code = compile(
        "float4 main(uint i : SV_VertexID) : SV_Position { return float4(i==1?3:-1,i==2?-3:1,0.5,1); }",
        "vs_5_0");
    ID3D11VertexShader *vs;
    check(ID3D11Device_CreateVertexShader(dev, ID3D10Blob_GetBufferPointer(code),
        ID3D10Blob_GetBufferSize(code), NULL, &vs));
    ID3D10Blob_Release(code);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    D3D11_VIEWPORT viewport = {0, 0, 1, 1, 0, 1};
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &viewport);
    ID3D11PixelShader *ps[4];
    for (UINT i = 0; i < 4; ++i) {
        code = compile(pixel_sources[i], "ps_5_0");
        check(ID3D11Device_CreatePixelShader(dev, ID3D10Blob_GetBufferPointer(code),
            ID3D10Blob_GetBufferSize(code), NULL, &ps[i]));
        ID3D10Blob_Release(code);
    }
    D3D11_BLEND_DESC blend_desc = {0};
    blend_desc.AlphaToCoverageEnable = TRUE;
    blend_desc.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL;
    ID3D11BlendState *blend;
    check(ID3D11Device_CreateBlendState(dev, &blend_desc, &blend));
    ID3D11Buffer *constants;
    D3D11_BUFFER_DESC cb_desc = {16, D3D11_USAGE_DEFAULT, D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0};
    check(ID3D11Device_CreateBuffer(dev, &cb_desc, NULL, &constants));
    ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &constants);
    D3D11_TEXTURE2D_DESC desc = {1, 1, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0},
        D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0};
    ID3D11Texture2D *resolved, *staging;
    check(ID3D11Device_CreateTexture2D(dev, &desc, NULL, &resolved));
    D3D11_TEXTURE2D_DESC map_desc = desc;
    map_desc.Usage = D3D11_USAGE_STAGING;
    map_desc.BindFlags = 0;
    map_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    check(ID3D11Device_CreateTexture2D(dev, &map_desc, NULL, &staging));
    for (UINT samples = 1; samples <= 4; samples *= 4) {
        desc.SampleDesc.Count = samples;
        ID3D11Texture2D *image;
        ID3D11RenderTargetView *view;
        check(ID3D11Device_CreateTexture2D(dev, &desc, NULL, &image));
        check(ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)image, NULL, &view));
        for (UINT i = 0; i < sizeof(tests) / sizeof(tests[0]); ++i) {
            const struct test *t = &tests[i];
            float color[4] = {0, 1, 0, t->alpha}, clear[4] = {0};
            memcpy(&color[2], &t->output, sizeof(t->output));
            ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)constants, 0, NULL, color, 0, 0);
            ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &view, NULL);
            ID3D11DeviceContext_ClearRenderTargetView(ctx, view, clear);
            ID3D11DeviceContext_PSSetShader(ctx, ps[t->shader], NULL, 0);
            ID3D11DeviceContext_OMSetBlendState(ctx, t->a2c ? blend : NULL, NULL, t->mask);
            ID3D11DeviceContext_Draw(ctx, 3, 0);
            ID3D11DeviceContext_OMSetRenderTargets(ctx, 0, NULL, NULL);
            if (samples > 1)
                ID3D11DeviceContext_ResolveSubresource(ctx, (ID3D11Resource *)resolved, 0,
                    (ID3D11Resource *)image, 0, desc.Format);
            ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging,
                (ID3D11Resource *)(samples > 1 ? resolved : image));
            D3D11_MAPPED_SUBRESOURCE mapped;
            check(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped));
            const unsigned char *rgba = mapped.pData;
            printf("%s\t%u\t%u\t%08x\t%08x\t%u\t%.1f\t%02x%02x%02x%02x\n",
                name, samples, t->shader, t->mask, t->output, t->a2c, t->alpha,
                rgba[0], rgba[1], rgba[2], rgba[3]);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        }
        ID3D11RenderTargetView_Release(view);
        ID3D11Texture2D_Release(image);
    }
    ID3D11Buffer_Release(constants);
    ID3D11BlendState_Release(blend);
    ID3D11Texture2D_Release(staging);
    ID3D11Texture2D_Release(resolved);
    for (UINT i = 0; i < 4; ++i) ID3D11PixelShader_Release(ps[i]);
    ID3D11VertexShader_Release(vs);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(dev);
}

int main(void)
{
    puts("driver\tsamples\tshader\tom_mask\tshader_mask\talpha_to_coverage\talpha\trgba");
    probe(D3D_DRIVER_TYPE_HARDWARE, "hardware");
    probe(D3D_DRIVER_TYPE_WARP, "warp");
    return 0;
}
