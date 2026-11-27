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

static HRESULT query_data(ID3D11DeviceContext *ctx, ID3D11Asynchronous *query, void *output, UINT size)
{
    HRESULT hr;
    do {
        hr = ID3D11DeviceContext_GetData(ctx, query, output, size, 0);
        if (hr == S_FALSE) Sleep(1);
    } while (hr == S_FALSE);
    return hr;
}

static ID3D11Query *begin_pipeline(ID3D11Device *device, ID3D11DeviceContext *ctx)
{
    D3D11_QUERY_DESC desc = {D3D11_QUERY_PIPELINE_STATISTICS, 0};
    ID3D11Query *query = NULL;
    HRESULT hr = ID3D11Device_CreateQuery(device, &desc, &query);
    if (SUCCEEDED(hr)) ID3D11DeviceContext_Begin(ctx, (ID3D11Asynchronous *)query);
    else printf("pipeline-create\t%08lx\n", (unsigned long)hr);
    return query;
}

static void end_pipeline(ID3D11DeviceContext *ctx, ID3D11Query *query, const char *label)
{
    if (!query) return;
    D3D11_QUERY_DATA_PIPELINE_STATISTICS s = {0};
    ID3D11DeviceContext_End(ctx, (ID3D11Asynchronous *)query);
    HRESULT hr = query_data(ctx, (ID3D11Asynchronous *)query, &s, sizeof(s));
    printf("%s\t%08lx\t%llu\t%llu\t%llu\t%llu\t%llu\t%llu\t%llu\t%llu\t%llu\t%llu\t%llu\n",
            label, (unsigned long)hr, s.IAVertices, s.IAPrimitives, s.VSInvocations,
            s.GSInvocations, s.GSPrimitives, s.CInvocations, s.CPrimitives,
            s.PSInvocations, s.HSInvocations, s.DSInvocations, s.CSInvocations);
    ID3D11Query_Release(query);
}

static void overflow_queries(ID3D11Device *device, ID3D11DeviceContext *ctx)
{
    ID3D11Predicate *predicates[5] = {0};
    const D3D11_QUERY kinds[] = {D3D11_QUERY_SO_OVERFLOW_PREDICATE,
        D3D11_QUERY_SO_OVERFLOW_PREDICATE_STREAM0, D3D11_QUERY_SO_OVERFLOW_PREDICATE_STREAM1,
        D3D11_QUERY_SO_OVERFLOW_PREDICATE_STREAM2, D3D11_QUERY_SO_OVERFLOW_PREDICATE_STREAM3};
    for (UINT i = 0; i < 5; ++i) {
        D3D11_QUERY_DESC desc = {kinds[i], 0};
        HRESULT hr = ID3D11Device_CreatePredicate(device, &desc, &predicates[i]);
        if (SUCCEEDED(hr)) ID3D11DeviceContext_Begin(ctx, (ID3D11Asynchronous *)predicates[i]);
        else printf("so-overflow-create-%u\t%08lx\n", i, (unsigned long)hr);
    }
    ID3D11DeviceContext_Draw(ctx, 3, 0);
    for (UINT i = 0; i < 5; ++i)
        if (predicates[i]) ID3D11DeviceContext_End(ctx, (ID3D11Asynchronous *)predicates[i]);
    ID3D11Query *stats = begin_pipeline(device, ctx);
    if (predicates[0]) {
        ID3D11DeviceContext_SetPredication(ctx, predicates[0], TRUE);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        ID3D11DeviceContext_SetPredication(ctx, predicates[0], FALSE);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        ID3D11DeviceContext_SetPredication(ctx, NULL, FALSE);
    }
    end_pipeline(ctx, stats, "so-overflow-predicated");
    for (UINT i = 0; i < 5; ++i) {
        if (!predicates[i]) continue;
        BOOL overflow = FALSE;
        HRESULT hr = query_data(ctx, (ID3D11Asynchronous *)predicates[i], &overflow, sizeof(overflow));
        printf("so-overflow-%u\t%08lx\t%d\n", i, (unsigned long)hr, (int)overflow);
        ID3D11Predicate_Release(predicates[i]);
    }
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
            D3D11_QUERY_DESC so_desc = {D3D11_QUERY_SO_STATISTICS, 0};
            ID3D11Query *so_query = NULL;
            if (SUCCEEDED(ID3D11Device_CreateQuery(device, &so_desc, &so_query))) {
                D3D11_QUERY_DATA_SO_STATISTICS stats = {0};
                ID3D11DeviceContext_Begin(ctx, (ID3D11Asynchronous *)so_query);
                ID3D11DeviceContext_Draw(ctx, 3, 0);
                ID3D11DeviceContext_End(ctx, (ID3D11Asynchronous *)so_query);
                while ((hr = ID3D11DeviceContext_GetData(ctx, (ID3D11Asynchronous *)so_query,
                        &stats, sizeof(stats), 0)) == S_FALSE) Sleep(1);
                printf("so-unbound\t%08lx\t%llu\t%llu\n", (unsigned long)hr,
                        (unsigned long long)stats.NumPrimitivesWritten,
                        (unsigned long long)stats.PrimitivesStorageNeeded);
                ID3D11Query_Release(so_query);
            }
            overflow_queries(device, ctx);
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

static void geometry_routing(ID3D11Device *device, ID3D11DeviceContext *ctx, int volume)
{
    ID3DBlob *vcode = compile(
            "float4 main(uint id:SV_VertexID):SV_Position {"
            "return float4(id == 1 ? 3 : -1, id == 2 ? -3 : 1, 0.5, 1); }", "vs_5_0");
    ID3DBlob *gcode = compile(
            "struct Out { float4 pos:SV_Position; uint viewport:SV_ViewportArrayIndex;"
            " uint layer:SV_RenderTargetArrayIndex; };"
            "[maxvertexcount(3)] void main(triangle float4 pos[3]:SV_Position,"
            " inout TriangleStream<Out> stream) {"
            " for (int i = 0; i < 3; ++i) { Out o; o.pos = pos[i]; o.viewport = 1;"
            " o.layer = 1; stream.Append(o); } }", "gs_5_0");
    ID3DBlob *pcode = compile("float4 main():SV_Target {return float4(1,0,0,1);}", "ps_5_0");
    ID3D11VertexShader *vs = NULL;
    ID3D11GeometryShader *gs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11Resource *target = NULL, *staging = NULL;
    ID3D11RenderTargetView *view = NULL;
    ID3D11RasterizerState *raster = NULL;
    if (!vcode || !gcode || !pcode) goto done;
    HRESULT hr = ID3D11Device_CreateVertexShader(device, ID3D10Blob_GetBufferPointer(vcode),
            ID3D10Blob_GetBufferSize(vcode), NULL, &vs);
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreateGeometryShader(device, ID3D10Blob_GetBufferPointer(gcode),
            ID3D10Blob_GetBufferSize(gcode), NULL, &gs);
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreatePixelShader(device, ID3D10Blob_GetBufferPointer(pcode),
            ID3D10Blob_GetBufferSize(pcode), NULL, &ps);
    D3D11_RASTERIZER_DESC raster_desc = {D3D11_FILL_SOLID, D3D11_CULL_NONE, FALSE,
        0, 0, 0, TRUE, TRUE, FALSE, FALSE};
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreateRasterizerState(device, &raster_desc, &raster);
    if (volume) {
        D3D11_TEXTURE3D_DESC desc = {8, 2, 8, 2, DXGI_FORMAT_R8G8B8A8_UNORM,
            D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0};
        ID3D11Texture3D *image = NULL, *readback = NULL;
        D3D11_RENDER_TARGET_VIEW_DESC rtv = {0};
        rtv.Format = desc.Format;
        rtv.ViewDimension = D3D11_RTV_DIMENSION_TEXTURE3D;
        rtv.Texture3D.MipSlice = 1;
        rtv.Texture3D.WSize = UINT_MAX;
        if (SUCCEEDED(hr)) hr = ID3D11Device_CreateTexture3D(device, &desc, NULL, &image);
        ID3D11RenderTargetView *whole = NULL;
        if (SUCCEEDED(hr)) hr = ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)image, &rtv, &whole);
        rtv.Texture3D.FirstWSlice = 1;
        rtv.Texture3D.WSize = 2;
        if (SUCCEEDED(hr)) hr = ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)image, &rtv, &view);
        desc.Width = 4;
        desc.Height = 1;
        desc.Depth = 4;
        desc.MipLevels = 1;
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        if (SUCCEEDED(hr)) hr = ID3D11Device_CreateTexture3D(device, &desc, NULL, &readback);
        const float blue[] = {0, 0, 1, 1}, green[] = {0, 1, 0, 1};
        if (SUCCEEDED(hr)) {
            ID3D11DeviceContext_ClearRenderTargetView(ctx, whole, blue);
            ID3D11DeviceContext_ClearRenderTargetView(ctx, view, green);
        }
        if (whole) ID3D11RenderTargetView_Release(whole);
        target = (ID3D11Resource *)image;
        staging = (ID3D11Resource *)readback;
    } else {
        D3D11_TEXTURE2D_DESC desc = {4, 1, 1, 2, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0},
            D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0};
        ID3D11Texture2D *image = NULL, *readback = NULL;
        UINT zero[4] = {0};
        D3D11_SUBRESOURCE_DATA initial[2] = {{zero, sizeof(zero), 0}, {zero, sizeof(zero), 0}};
        if (SUCCEEDED(hr)) hr = ID3D11Device_CreateTexture2D(device, &desc, initial, &image);
        if (SUCCEEDED(hr)) hr = ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)image, NULL, &view);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        if (SUCCEEDED(hr)) hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &readback);
        target = (ID3D11Resource *)image;
        staging = (ID3D11Resource *)readback;
    }
    ID3D11Query *stats = NULL;
    printf("%s\t%08lx", volume ? "geometry-w-slices" : "geometry-routing", (unsigned long)hr);
    if (SUCCEEDED(hr)) {
        D3D11_VIEWPORT viewports[2] = {{0, 0, 2, 1, 0, 1}, {2, 0, 2, 1, 0, 1}};
        D3D11_RECT scissors[2] = {{0, 0, 2, 1}, {3, 0, 4, 1}};
        ID3D11DeviceContext_RSSetState(ctx, raster);
        ID3D11DeviceContext_RSSetViewports(ctx, 2, viewports);
        ID3D11DeviceContext_RSSetScissorRects(ctx, 2, scissors);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_GSSetShader(ctx, gs, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &view, NULL);
        stats = begin_pipeline(device, ctx);
        ID3D11DeviceContext_Draw(ctx, 3, 0);
        if (volume) ID3D11DeviceContext_CopySubresourceRegion(ctx, staging, 0, 0, 0, 0, target, 1, NULL);
        else ID3D11DeviceContext_CopyResource(ctx, staging, target);
        for (UINT slice = 0; slice < (volume ? 1u : 2u); ++slice) {
            D3D11_MAPPED_SUBRESOURCE mapped;
            hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, slice, D3D11_MAP_READ, 0, &mapped);
            printf("\t%08lx", (unsigned long)hr);
            if (SUCCEEDED(hr)) {
                for (UINT w = 0; w < (volume ? 4u : 1u); ++w) {
                    const UINT *pixels = (const UINT *)((const char *)mapped.pData + w * mapped.DepthPitch);
                    for (UINT pixel = 0; pixel < 4; ++pixel) printf("\t%08x", pixels[pixel]);
                }
                ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, slice);
            }
        }
    }
    printf("\n");
    end_pipeline(ctx, stats, volume ? "pipeline-volume" : "pipeline-geometry");
    ID3D11DeviceContext_ClearState(ctx);
done:
    if (raster) ID3D11RasterizerState_Release(raster);
    if (view) ID3D11RenderTargetView_Release(view);
    if (target) ID3D11Resource_Release(target);
    if (staging) ID3D11Resource_Release(staging);
    if (vs) ID3D11VertexShader_Release(vs);
    if (gs) ID3D11GeometryShader_Release(gs);
    if (ps) ID3D11PixelShader_Release(ps);
    if (vcode) ID3D10Blob_Release(vcode);
    if (gcode) ID3D10Blob_Release(gcode);
    if (pcode) ID3D10Blob_Release(pcode);
}

static void tessellation_and_compute(ID3D11Device *device, ID3D11DeviceContext *ctx)
{
    ID3DBlob *vs_code = compile(
            "float4 main(uint id:SV_VertexID):SV_Position {"
            "return float4(id == 1 ? 3 : -1, id == 2 ? -3 : 1, 0.5, 1); }", "vs_5_0");
    ID3DBlob *hs_code = compile(
            "struct V {float4 p:SV_Position;};"
            "struct P {float e[3]:SV_TessFactor;float i:SV_InsideTessFactor;};"
            "P factors(InputPatch<V,3> p){P o={{64,64,64},64};return o;}"
            "[domain(\"tri\")][partitioning(\"fractional_even\")][outputtopology(\"triangle_cw\")]"
            "[outputcontrolpoints(3)][patchconstantfunc(\"factors\")]"
            "V main(InputPatch<V,3> p,uint id:SV_OutputControlPointID){return p[id];}", "hs_5_0");
    ID3DBlob *ds_code = compile(
            "struct V {float4 p:SV_Position;};"
            "struct P {float e[3]:SV_TessFactor;float i:SV_InsideTessFactor;};"
            "[domain(\"tri\")] float4 main(P f,float3 u:SV_DomainLocation,const OutputPatch<V,3> p):SV_Position"
            "{return p[0].p*u.x+p[1].p*u.y+p[2].p*u.z;}", "ds_5_0");
    ID3DBlob *gs_code = compile(
            "struct V {float4 p:SV_Position;};"
            "[maxvertexcount(64)] void main(triangle V p[3],inout TriangleStream<V> s)"
            "{[unroll]for(uint i=0;i<3;i++)s.Append(p[i]);s.RestartStrip();}", "gs_5_0");
    ID3DBlob *cs_code = compile("[numthreads(8,4,1)] void main(){}", "cs_5_0");
    ID3D11VertexShader *vs = NULL;
    ID3D11HullShader *hs = NULL;
    ID3D11DomainShader *ds = NULL;
    ID3D11GeometryShader *gs = NULL;
    ID3D11ComputeShader *cs = NULL;
    ID3D11Buffer *output = NULL, *args = NULL;
    ID3D11Query *so = NULL;
    if (!vs_code || !hs_code || !ds_code || !gs_code || !cs_code) goto done;
#define BLOB_ARGUMENTS(code) ID3D10Blob_GetBufferPointer(code), ID3D10Blob_GetBufferSize(code)
    HRESULT hr = device->lpVtbl->CreateVertexShader(device, BLOB_ARGUMENTS(vs_code), NULL, &vs);
    if (SUCCEEDED(hr)) hr = device->lpVtbl->CreateHullShader(device, BLOB_ARGUMENTS(hs_code), NULL, &hs);
    if (SUCCEEDED(hr)) hr = device->lpVtbl->CreateDomainShader(device, BLOB_ARGUMENTS(ds_code), NULL, &ds);
    const D3D11_SO_DECLARATION_ENTRY entry = {0, "SV_Position", 0, 0, 4, 0};
    UINT stride = 16;
    if (SUCCEEDED(hr)) hr = device->lpVtbl->CreateGeometryShaderWithStreamOutput(device, BLOB_ARGUMENTS(gs_code),
            &entry, 1, &stride, 1, D3D11_SO_NO_RASTERIZED_STREAM, NULL, &gs);
    if (SUCCEEDED(hr)) hr = device->lpVtbl->CreateComputeShader(device, BLOB_ARGUMENTS(cs_code), NULL, &cs);
#undef BLOB_ARGUMENTS
    D3D11_BUFFER_DESC desc = {16 * 18432, D3D11_USAGE_DEFAULT, D3D11_BIND_STREAM_OUTPUT, 0, 0, 0};
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreateBuffer(device, &desc, NULL, &output);
    const UINT groups[] = {0, 3, 2, 1};
    D3D11_SUBRESOURCE_DATA data = {groups, 0, 0};
    desc.ByteWidth = sizeof(groups);
    desc.BindFlags = 0;
    desc.MiscFlags = D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS;
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreateBuffer(device, &desc, &data, &args);
    const D3D11_QUERY_DESC query_desc = {D3D11_QUERY_SO_STATISTICS, 0};
    if (SUCCEEDED(hr)) hr = ID3D11Device_CreateQuery(device, &query_desc, &so);
    printf("tess-gs-create\t%08lx\n", (unsigned long)hr);
    if (FAILED(hr)) goto done;

    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_HSSetShader(ctx, hs, NULL, 0);
    ID3D11DeviceContext_DSSetShader(ctx, ds, NULL, 0);
    ID3D11DeviceContext_GSSetShader(ctx, gs, NULL, 0);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST);
    UINT offset = 0;
    ID3D11DeviceContext_SOSetTargets(ctx, 1, &output, &offset);
    ID3D11DeviceContext_Begin(ctx, (ID3D11Asynchronous *)so);
    ID3D11Query *pipeline = begin_pipeline(device, ctx);
    ID3D11DeviceContext_Draw(ctx, 3, 0);
    ID3D11DeviceContext_End(ctx, (ID3D11Asynchronous *)so);
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_Dispatch(ctx, 1, 1, 1);
    ID3D11DeviceContext_DispatchIndirect(ctx, args, 4);
    end_pipeline(ctx, pipeline, "pipeline-tess-gs-cs");
    D3D11_QUERY_DATA_SO_STATISTICS stats = {0};
    hr = query_data(ctx, (ID3D11Asynchronous *)so, &stats, sizeof(stats));
    printf("tess-gs-so\t%08lx\t%llu\t%llu\n", (unsigned long)hr,
            stats.NumPrimitivesWritten, stats.PrimitivesStorageNeeded);
    ID3D11DeviceContext_ClearState(ctx);

done:
    if (so) ID3D11Query_Release(so);
    if (args) ID3D11Buffer_Release(args);
    if (output) ID3D11Buffer_Release(output);
    if (cs) ID3D11ComputeShader_Release(cs);
    if (gs) ID3D11GeometryShader_Release(gs);
    if (ds) ID3D11DomainShader_Release(ds);
    if (hs) ID3D11HullShader_Release(hs);
    if (vs) ID3D11VertexShader_Release(vs);
    if (cs_code) ID3D10Blob_Release(cs_code);
    if (gs_code) ID3D10Blob_Release(gs_code);
    if (ds_code) ID3D10Blob_Release(ds_code);
    if (hs_code) ID3D10Blob_Release(hs_code);
    if (vs_code) ID3D10Blob_Release(vs_code);
}

static void class_data(ID3D11Device *device, int resources)
{
    static const char constant_source[] =
            "interface Color { float4 shade(); };"
            "class Tint : Color { float4 tint; float4 shade() { return tint; } };"
            "Color paint; Tint namedTint[2];"
            "float4 main() : SV_Target { return paint.shade(); }";
    static const char resource_source[] =
            "interface Color { float4 shade(); };"
            "class Tint : Color { float4 tint; Texture2D<float4> tex; SamplerState samp;"
            " float4 shade() { return tint * tex.Sample(samp, float2(0.5, 0.5)); } };"
            "Color paint; Tint namedTint[2];"
            "float4 main() : SV_Target { return paint.shade(); }";
    const char *label = resources ? "class-resources" : "class-data";
    ID3DBlob *code = compile(resources ? resource_source : constant_source, "ps_5_0");
    if (!code) return;
    ID3D11ClassLinkage *linkage = NULL;
    ID3D11PixelShader *shader = NULL;
    HRESULT hr = ID3D11Device_CreateClassLinkage(device, &linkage);
    if (SUCCEEDED(hr))
        hr = ID3D11Device_CreatePixelShader(device, ID3D10Blob_GetBufferPointer(code),
                ID3D10Blob_GetBufferSize(code), linkage, &shader);
    printf("%s-create\t%08lx\n", label, (unsigned long)hr);
    if (SUCCEEDED(hr)) {
        ID3D11ClassInstance *instances[4] = {NULL, NULL, NULL, NULL};
        HRESULT status[4];
        status[0] = ID3D11ClassLinkage_GetClassInstance(linkage, "namedTint", 0, &instances[0]);
        status[1] = ID3D11ClassLinkage_GetClassInstance(linkage, "namedTint", 1, &instances[1]);
        status[2] = ID3D11ClassLinkage_GetClassInstance(linkage, "namedTint", 2, &instances[2]);
        status[3] = ID3D11ClassLinkage_CreateClassInstance(linkage, "Tint", 3, 4,
                resources ? 5 : 0, resources ? 2 : 0, &instances[3]);
        for (unsigned i = 0; i < 4; ++i) {
            printf("%s-desc-%u\t%08lx", label, i, (unsigned long)status[i]);
            if (instances[i]) {
                D3D11_CLASS_INSTANCE_DESC desc;
                ID3D11ClassInstance_GetDesc(instances[i], &desc);
                printf("\t%u\t%u\t%u\t%u\t%u\t%u\t%u\t%d", desc.InstanceId, desc.InstanceIndex,
                       desc.TypeId, desc.ConstantBuffer, desc.BaseConstantBufferOffset,
                       desc.BaseTexture, desc.BaseSampler, (int)desc.Created);
                char instance[32] = {0}, type[32] = {0}, short_name[2] = {'?', 0};
                SIZE_T instance_size = 0, type_size = 0, short_size = sizeof(short_name);
                ID3D11ClassInstance_GetInstanceName(instances[i], NULL, &instance_size);
                ID3D11ClassInstance_GetTypeName(instances[i], NULL, &type_size);
                ID3D11ClassInstance_GetInstanceName(instances[i], short_name, &short_size);
                printf("\t%u\t%u\t%u\t%02x", (unsigned)instance_size, (unsigned)type_size,
                        (unsigned)short_size, (unsigned char)short_name[0]);
                instance_size = sizeof(instance);
                type_size = sizeof(type);
                ID3D11ClassInstance_GetInstanceName(instances[i], instance, &instance_size);
                ID3D11ClassInstance_GetTypeName(instances[i], type, &type_size);
                printf("\t%s\t%s", instance, type);
                ID3D11ClassInstance_Release(instances[i]);
            }
            printf("\n");
        }
        ID3DBlob *text = NULL;
        if (SUCCEEDED(D3DDisassemble(ID3D10Blob_GetBufferPointer(code), ID3D10Blob_GetBufferSize(code),
                0, NULL, &text))) {
            printf("%s-disassembly\n%s\n", label, (const char *)ID3D10Blob_GetBufferPointer(text));
            ID3D10Blob_Release(text);
        }
        const unsigned char *bytes = ID3D10Blob_GetBufferPointer(code);
        printf("%s-dxbc\t", label);
        for (SIZE_T i = 0; i < ID3D10Blob_GetBufferSize(code); ++i) printf("%02x", bytes[i]);
        printf("\n");
    }
    if (shader) ID3D11PixelShader_Release(shader);
    if (linkage) ID3D11ClassLinkage_Release(linkage);
    ID3D10Blob_Release(code);
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
    geometry_routing(device, ctx, 0);
    geometry_routing(device, ctx, 1);
    tessellation_and_compute(device, ctx);
    class_data(device, 0);
    class_data(device, 1);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(device);
    return 0;
}
