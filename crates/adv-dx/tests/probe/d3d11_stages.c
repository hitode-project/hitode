/* Supplemental Windows stage measurements.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o d3d11_stages.exe d3d11_stages.c -ld3d11 -ld3dcompiler
 */
#define COBJMACROS
#include <d3d11.h>
#include <d3dcompiler.h>
#include <stdio.h>
#include <string.h>

static ID3DBlob *compile(const char *source, const char *profile)
{
    ID3DBlob *bytecode = NULL, *errors = NULL;
    HRESULT hr = D3DCompile(source, strlen(source), NULL, NULL, NULL, "main", profile,
            D3DCOMPILE_ENABLE_STRICTNESS, 0, &bytecode, &errors);
    if (FAILED(hr))
        printf("compile-%s\t%08lx\t%s\n", profile, (unsigned long)hr,
                errors ? (const char *)ID3D10Blob_GetBufferPointer(errors) : "");
    if (errors) ID3D10Blob_Release(errors);
    return bytecode;
}

static void stream_output(ID3D11Device *device, ID3D11DeviceContext *ctx)
{
    static const char source[] =
        "float4 main(uint id : SV_VertexID) : SV_Position { return float4(id, 1, 0.5, 1); }";
    ID3DBlob *code = compile(source, "vs_5_0");
    if (!code) return;
    void *bytes = ID3D10Blob_GetBufferPointer(code);
    SIZE_T length = ID3D10Blob_GetBufferSize(code);
    ID3D11VertexShader *vs = NULL;
    ID3D11GeometryShader *gs = NULL;
    D3D11_SO_DECLARATION_ENTRY entry = {0, "SV_Position", 0, 0, 4, 0};
    UINT stride = 16;
    HRESULT hr = ID3D11Device_CreateVertexShader(device, bytes, length, NULL, &vs);
    HRESULT so = ID3D11Device_CreateGeometryShaderWithStreamOutput(device, bytes, length,
            &entry, 1, &stride, 1, D3D11_SO_NO_RASTERIZED_STREAM, NULL, &gs);
    printf("so-create\t%08lx\t%08lx\n", (unsigned long)hr, (unsigned long)so);
    ID3D10Blob_Release(code);
    if (SUCCEEDED(hr) && SUCCEEDED(so)) {
        ID3D11Buffer *buffer = NULL, *staging = NULL, *bound = NULL;
        UINT initial[28] = {0};
        D3D11_SUBRESOURCE_DATA data = {initial, 0, 0};
        D3D11_BUFFER_DESC desc = {sizeof(initial), D3D11_USAGE_DEFAULT,
            D3D11_BIND_VERTEX_BUFFER | D3D11_BIND_STREAM_OUTPUT, 0, 0, 0};
        hr = ID3D11Device_CreateBuffer(device, &desc, &data, &buffer);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        HRESULT map = ID3D11Device_CreateBuffer(device, &desc, NULL, &staging);
        printf("so-buffers\t%08lx\t%08lx\n", (unsigned long)hr, (unsigned long)map);
        if (buffer && staging) {
            UINT offset = 16;
            ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
            ID3D11DeviceContext_GSSetShader(ctx, gs, NULL, 0);
            ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_POINTLIST);
            ID3D11DeviceContext_SOSetTargets(ctx, 1, &buffer, &offset);
            ID3D11DeviceContext_SOGetTargets(ctx, 1, &bound);
            printf("so-get\t%u\n", bound == buffer);
            if (bound) ID3D11Buffer_Release(bound);
            ID3D11DeviceContext_Draw(ctx, 3, 0);
            offset = UINT_MAX;
            ID3D11DeviceContext_SOSetTargets(ctx, 1, &buffer, &offset);
            ID3D11DeviceContext_Draw(ctx, 3, 0);
            ID3D11DeviceContext_SOSetTargets(ctx, 0, NULL, NULL);
            ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)buffer);
            D3D11_MAPPED_SUBRESOURCE mapped;
            hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
            printf("so-data\t%08lx", (unsigned long)hr);
            if (SUCCEEDED(hr)) {
                for (UINT i = 0; i < 28; ++i) printf("\t%08x", ((UINT *)mapped.pData)[i]);
                ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
            }
            printf("\n");
            ID3D11DeviceContext_GSSetShader(ctx, NULL, NULL, 0);
            offset = 16;
            ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &buffer, &stride, &offset);
            D3D11_QUERY_DESC query_desc = {D3D11_QUERY_PIPELINE_STATISTICS, 0};
            ID3D11Query *query = NULL;
            if (SUCCEEDED(ID3D11Device_CreateQuery(device, &query_desc, &query))) {
                D3D11_QUERY_DATA_PIPELINE_STATISTICS stats = {0};
                ID3D11DeviceContext_Begin(ctx, (ID3D11Asynchronous *)query);
                ID3D11DeviceContext_DrawAuto(ctx);
                ID3D11DeviceContext_End(ctx, (ID3D11Asynchronous *)query);
                do {
                    hr = ID3D11DeviceContext_GetData(ctx, (ID3D11Asynchronous *)query, &stats, sizeof(stats), 0);
                } while (hr == S_FALSE);
                printf("draw-auto\t%08lx\t%llu\n", (unsigned long)hr,
                        (unsigned long long)stats.IAVertices);
                ID3D11Query_Release(query);
            }
        }
        if (staging) ID3D11Buffer_Release(staging);
        if (buffer) ID3D11Buffer_Release(buffer);
        ID3D11DeviceContext_ClearState(ctx);
    }
    if (gs) ID3D11GeometryShader_Release(gs);
    if (vs) ID3D11VertexShader_Release(vs);
}

static void pixel_uav(ID3D11Device *device, ID3D11DeviceContext *ctx)
{
    ID3DBlob *vcode = compile(
            "float4 main(uint id:SV_VertexID):SV_Position {"
            "return float4(id == 1 ? 3 : -1, id == 2 ? -3 : 1, 0.5, 1); }", "vs_5_0");
    ID3DBlob *pcode = compile(
            "RWStructuredBuffer<uint> output : register(u0);"
            "void main(float4 p:SV_Position) { output[uint(p.x)] = uint(p.x) + 17; }", "ps_5_0");
    if (!vcode || !pcode) goto done;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Buffer *values = NULL, *staging = NULL;
    ID3D11UnorderedAccessView *view = NULL;
    D3D11_BUFFER_DESC desc = {16, D3D11_USAGE_DEFAULT,
        D3D11_BIND_UNORDERED_ACCESS, 0, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 4};
    HRESULT hr = ID3D11Device_CreateVertexShader(device, ID3D10Blob_GetBufferPointer(vcode),
            ID3D10Blob_GetBufferSize(vcode), NULL, &vs);
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreatePixelShader(device, ID3D10Blob_GetBufferPointer(pcode),
            ID3D10Blob_GetBufferSize(pcode), NULL, &ps);
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreateBuffer(device, &desc, NULL, &values);
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreateUnorderedAccessView(device, (ID3D11Resource *)values, NULL, &view);
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = desc.MiscFlags = desc.StructureByteStride = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreateBuffer(device, &desc, NULL, &staging);
    printf("pixel-uav\t%08lx", (unsigned long)hr);
    if (SUCCEEDED(hr)) {
        D3D11_VIEWPORT viewport = {0, 0, 4, 1, 0, 1};
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &viewport);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
        ID3D11DeviceContext_OMSetRenderTargetsAndUnorderedAccessViews(ctx, 0, NULL, NULL, 0, 1, &view, NULL);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)values);
        D3D11_MAPPED_SUBRESOURCE mapped;
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped);
        printf("\t%08lx", (unsigned long)hr);
        if (SUCCEEDED(hr)) {
            for (UINT i = 0; i < 4; ++i) printf("\t%u", ((UINT *)mapped.pData)[i]);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
        }
    }
    printf("\n");
    ID3D11DeviceContext_ClearState(ctx);
    if (staging) ID3D11Buffer_Release(staging);
    if (view) ID3D11UnorderedAccessView_Release(view);
    if (values) ID3D11Buffer_Release(values);
    if (vs) ID3D11VertexShader_Release(vs);
    if (ps) ID3D11PixelShader_Release(ps);
done:
    if (vcode) ID3D10Blob_Release(vcode);
    if (pcode) ID3D10Blob_Release(pcode);
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
    stream_output(device, ctx);
    pixel_uav(device, ctx);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(device);
    return 0;
}
