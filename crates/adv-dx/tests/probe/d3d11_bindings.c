/* Supplemental FL 11_0 binding measurements, hardware and WARP.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o d3d11_bindings.exe d3d11_bindings.c -ld3d11 -ld3dcompiler
 */
#define COBJMACROS
#include <d3d11.h>
#include <d3dcompiler.h>
#include <stdio.h>
#include <string.h>

enum { CBS = 14, SRVS = 128, SAMPLERS = 16, UAVS = 8 };

static HRESULT dispatch_limits(ID3D11Device *device, ID3D11DeviceContext *ctx)
{
    static UINT cb_data[65536 / sizeof(UINT)];
    char source[32768], *at = source;
    ID3D11Buffer *cb[CBS] = {0};
    ID3D11Texture2D *input[SRVS] = {0}, *output[UAVS] = {0}, *staging = NULL;
    ID3D11ShaderResourceView *srv[SRVS] = {0};
    ID3D11SamplerState *samplers[SAMPLERS] = {0};
    ID3D11UnorderedAccessView *uav[UAVS] = {0};
    ID3D11ComputeShader *shader = NULL;
    ID3DBlob *code = NULL, *errors = NULL;
    D3D11_BUFFER_DESC cb_desc = {sizeof(cb_data), D3D11_USAGE_DEFAULT,
        D3D11_BIND_CONSTANT_BUFFER, 0, 0, 0};
    D3D11_SUBRESOURCE_DATA init_cb = {cb_data, 0, 0};
    D3D11_TEXTURE2D_DESC desc = {1, 1, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0},
        D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, 0};
    D3D11_SAMPLER_DESC sampler_desc = {D3D11_FILTER_MIN_MAG_MIP_POINT,
        D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE_ADDRESS_CLAMP,
        0.f, 1, D3D11_COMPARISON_NEVER, {0}, 0.f, D3D11_FLOAT32_MAX};
    HRESULT hr;

#define CHECK(row, call) do { \
    hr = (call); \
    if (FAILED(hr)) { printf("%s\t%08lx\n", row, (unsigned long)hr); goto done; } \
} while (0)
    for (unsigned i = 0; i < CBS; ++i) {
        cb_data[4095 * 4] = 100 + i;
        CHECK("constant-buffer", ID3D11Device_CreateBuffer(device, &cb_desc, &init_cb, &cb[i]));
        at += sprintf(at, "cbuffer cb%u : register(b%u) { uint4 c%u[4096]; };\n", i, i, i);
    }
    for (unsigned i = 0; i < SRVS; ++i) {
        unsigned char texel[4] = {i + 1, 0, 0, 255};
        D3D11_SUBRESOURCE_DATA init = {texel, 4, 4};
        CHECK("input-texture", ID3D11Device_CreateTexture2D(device, &desc, &init, &input[i]));
        CHECK("input-view", ID3D11Device_CreateShaderResourceView(device, (ID3D11Resource *)input[i],
            NULL, &srv[i]));
        at += sprintf(at, "Texture2D<float4> t%u : register(t%u);\n", i, i);
    }
    for (unsigned i = 0; i < SAMPLERS; ++i) {
        CHECK("sampler", ID3D11Device_CreateSamplerState(device, &sampler_desc, &samplers[i]));
        at += sprintf(at, "SamplerState s%u : register(s%u);\n", i, i);
    }
    desc.Format = DXGI_FORMAT_R32_UINT;
    desc.BindFlags = D3D11_BIND_UNORDERED_ACCESS;
    for (unsigned i = 0; i < UAVS; ++i) {
        CHECK("output-texture", ID3D11Device_CreateTexture2D(device, &desc, NULL, &output[i]));
        CHECK("output-view", ID3D11Device_CreateUnorderedAccessView(device,
            (ID3D11Resource *)output[i], NULL, &uav[i]));
        at += sprintf(at, "RWTexture2D<uint> u%u : register(u%u);\n", i, i);
    }
    desc.BindFlags = 0;
    desc.Usage = D3D11_USAGE_STAGING;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    CHECK("staging", ID3D11Device_CreateTexture2D(device, &desc, NULL, &staging));
    at += sprintf(at, "[numthreads(1,1,1)] void main() { uint total = 0;\n");
    for (unsigned i = 0; i < CBS; ++i)
        at += sprintf(at, "total += c%u[4095].x;\n", i);
    for (unsigned i = 0; i < SRVS; ++i) {
        if (i < SAMPLERS || (i >= 88 && i < 96))
            at += sprintf(at, "total += (uint)round(t%u.SampleLevel(s%u, float2(.5,.5), 0).r * 255);\n",
                i, i % SAMPLERS);
        else
            at += sprintf(at, "total += (uint)round(t%u.Load(int3(0,0,0)).r * 255);\n", i);
    }
    for (unsigned i = 0; i < UAVS; ++i)
        at += sprintf(at, "u%u[uint2(0,0)] = total + %u;\n", i, i);
    *at++ = '}';
    hr = D3DCompile(source, at - source, NULL, NULL, NULL, "main", "cs_5_0",
        D3DCOMPILE_ENABLE_STRICTNESS, 0, &code, &errors);
    printf("binding-shader\t%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) {
        if (errors) puts(ID3D10Blob_GetBufferPointer(errors));
        goto done;
    }
    CHECK("shader-create", ID3D11Device_CreateComputeShader(device,
        ID3D10Blob_GetBufferPointer(code), ID3D10Blob_GetBufferSize(code), NULL, &shader));
    ID3D11DeviceContext_CSSetConstantBuffers(ctx, 0, CBS, cb);
    ID3D11DeviceContext_CSSetShaderResources(ctx, 0, SRVS, srv);
    ID3D11DeviceContext_CSSetSamplers(ctx, 0, SAMPLERS, samplers);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, 0, UAVS, uav, NULL);
    ID3D11DeviceContext_CSSetShader(ctx, shader, NULL, 0);
    ID3D11DeviceContext_Dispatch(ctx, 1, 1, 1);
    ID3D11DeviceContext_ClearState(ctx);
    for (unsigned i = 0; i < UAVS; ++i) {
        D3D11_MAPPED_SUBRESOURCE mapped;
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)output[i]);
        CHECK("output-map", ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0,
            D3D11_MAP_READ, 0, &mapped));
        printf("binding-uav-%u\t%u\n", i, *(const UINT *)mapped.pData);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
    }

done:
    if (shader) ID3D11ComputeShader_Release(shader);
    if (code) ID3D10Blob_Release(code);
    if (errors) ID3D10Blob_Release(errors);
    if (staging) ID3D11Texture2D_Release(staging);
    for (unsigned i = 0; i < CBS; ++i)
        if (cb[i]) ID3D11Buffer_Release(cb[i]);
    for (unsigned i = 0; i < SRVS; ++i) {
        if (srv[i]) ID3D11ShaderResourceView_Release(srv[i]);
        if (input[i]) ID3D11Texture2D_Release(input[i]);
    }
    for (unsigned i = 0; i < SAMPLERS; ++i)
        if (samplers[i]) ID3D11SamplerState_Release(samplers[i]);
    for (unsigned i = 0; i < UAVS; ++i) {
        if (uav[i]) ID3D11UnorderedAccessView_Release(uav[i]);
        if (output[i]) ID3D11Texture2D_Release(output[i]);
    }
    return hr;
#undef CHECK
}

static HRESULT ia_slots(ID3D11Device *device, ID3D11DeviceContext *ctx)
{
    enum { STREAMS = 32 };
    ID3D11Buffer *buffer = NULL, *inputs[STREAMS], *bound[STREAMS] = {0};
    UINT offsets[STREAMS], strides[STREAMS], got_offsets[STREAMS], got_strides[STREAMS];
    D3D11_BUFFER_DESC desc = {STREAMS * 16, D3D11_USAGE_DEFAULT,
        D3D11_BIND_VERTEX_BUFFER, 0, 0, 0};
    HRESULT hr = ID3D11Device_CreateBuffer(device, &desc, NULL, &buffer);
    printf("ia-buffer\t%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) return hr;
    for (UINT i = 0; i < STREAMS; ++i) {
        inputs[i] = buffer;
        offsets[i] = i * 16;
        strides[i] = i + 1;
    }
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, STREAMS, inputs, strides, offsets);
    ID3D11DeviceContext_IAGetVertexBuffers(ctx, 0, STREAMS, bound, got_strides, got_offsets);
    for (UINT i = 0; i < STREAMS; ++i) {
        printf("ia-slot-%u\t%d\t%u\t%u\n", i, bound[i] == buffer, got_strides[i], got_offsets[i]);
        if (bound[i]) ID3D11Buffer_Release(bound[i]);
    }
    ID3D11DeviceContext_ClearState(ctx);
    ID3D11Buffer_Release(buffer);
    return hr;
}

int main(int argc, char **argv)
{
    D3D_DRIVER_TYPE driver = argc > 1 && !strcmp(argv[1], "warp")
        ? D3D_DRIVER_TYPE_WARP : D3D_DRIVER_TYPE_HARDWARE;
    const D3D_FEATURE_LEVEL level = D3D_FEATURE_LEVEL_11_0;
    ID3D11Device *device = NULL;
    ID3D11DeviceContext *ctx = NULL;
    HRESULT hr = D3D11CreateDevice(NULL, driver, NULL, 0, &level, 1,
        D3D11_SDK_VERSION, &device, NULL, &ctx);
    printf("device\t%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) return 1;
    hr = dispatch_limits(device, ctx);
    HRESULT ia = ia_slots(device, ctx);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(device);
    return FAILED(hr) || FAILED(ia);
}
