/* Windows hardware and WARP oracle.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o d3d11_descriptors.exe d3d11_descriptors.c -ld3d11
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>

static ID3D11Device *device;
static unsigned char data[1 << 20];
static const D3D11_SUBRESOURCE_DATA initial = {data, 256, 4096};
static D3D11_BUFFER_DESC buffer = {64, D3D11_USAGE_DEFAULT};
static D3D11_TEXTURE2D_DESC texture = {16, 16, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0}};

static void buf(const char *name, D3D11_BUFFER_DESC desc, int init)
{
    printf("request\t%u\t%u\t%u\t%u\t%u\t%u\t%d\n", desc.ByteWidth, desc.Usage,
           desc.BindFlags, desc.CPUAccessFlags, desc.MiscFlags, desc.StructureByteStride, init);
    ID3D11Buffer *resource = NULL;
    HRESULT result = ID3D11Device_CreateBuffer(device, &desc, init ? &initial : NULL, &resource);
    printf("buffer\t%s\t%08lx", name, (unsigned long)result);
    if (resource) {
        ID3D11Buffer_GetDesc(resource, &desc);
        printf("\t%u", desc.StructureByteStride);
        ID3D11Buffer_Release(resource);
    }
    puts("");
}

static void tex(const char *name, D3D11_TEXTURE2D_DESC desc, const D3D11_SUBRESOURCE_DATA *init)
{
    printf("request\t%u\t%u\t%u\t%u\t%u\t%u\t%u\t%u\t%u\t%u\t%u\t%d\n", desc.Width, desc.Height,
           desc.MipLevels, desc.ArraySize, desc.Format, desc.SampleDesc.Count, desc.SampleDesc.Quality,
           desc.Usage, desc.BindFlags, desc.CPUAccessFlags, desc.MiscFlags, init != NULL);
    ID3D11Texture2D *resource = NULL;
    HRESULT result = ID3D11Device_CreateTexture2D(device, &desc, init, &resource);
    printf("texture\t%s\t%08lx", name, (unsigned long)result);
    if (resource) {
        ID3D11Texture2D_GetDesc(resource, &desc);
        printf("\t%u", desc.MipLevels);
        ID3D11Texture2D_Release(resource);
    }
    puts("");
}

#define B(name, init, changes) do { D3D11_BUFFER_DESC d = buffer; changes; buf(name, d, init); } while (0)
#define T(name, init, changes) do { D3D11_TEXTURE2D_DESC d = texture; changes; tex(name, d, init); } while (0)

static void views(void)
{
    enum { SRV, RTV, UAV };
    static const struct { const char *name; unsigned kind, format, dimension, words[4]; } rows[] = {
        {"srv-null", SRV, 0, 0}, {"srv-unknown", SRV, 0, 5, {0, 1, 0, 2}},
        {"srv-non-array-dimension", SRV, 28, 4, {0, 1}},
        {"srv-other-typed-format", SRV, 29, 5, {0, 1, 0, 2}},
        {"srv-all-remaining", SRV, 28, 5, {0, ~0u, 1, ~0u}},
        {"srv-zero-mips", SRV, 28, 5, {0, 0, 0, 2}},
        {"rtv-null", RTV, 0, 0}, {"rtv-unknown", RTV, 0, 5, {0, 0, 2}},
        {"rtv-non-array-dimension", RTV, 28, 4, {0}},
        {"rtv-all-remaining", RTV, 28, 5, {0, 0, ~0u}},
        {"uav-null", UAV, 0, 0}, {"uav-unknown", UAV, 0, 5, {0, 0, 2}},
        {"uav-all-remaining", UAV, 28, 5, {0, 0, ~0u}},
    };
    D3D11_TEXTURE2D_DESC d = texture;
    ID3D11Texture2D *image = NULL;
    d.ArraySize = 2;
    d.MipLevels = 3;
    d.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET | D3D11_BIND_UNORDERED_ACCESS;
    if (FAILED(ID3D11Device_CreateTexture2D(device, &d, NULL, &image))) return;
    for (unsigned i = 0; i < sizeof(rows)/sizeof(*rows); ++i) {
        unsigned desc[6] = {rows[i].format, rows[i].dimension};
        memcpy(desc+2, rows[i].words, sizeof(rows[i].words));
        printf("request\t%u", rows[i].kind);
        for (unsigned j=0; j < 6; ++j) printf("\t%u", desc[j]);
        puts("");
        void *p = rows[i].dimension ? desc : NULL;
        IUnknown *view = NULL;
        HRESULT hr;
        if (rows[i].kind == SRV) {
            hr = ID3D11Device_CreateShaderResourceView(device, (ID3D11Resource *)image, p, (ID3D11ShaderResourceView **)&view);
            memset(desc, 0, sizeof(desc));
            if (view) ID3D11ShaderResourceView_GetDesc((ID3D11ShaderResourceView *)view, (void *)desc);
        } else if (rows[i].kind == RTV) {
            hr = ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)image, p, (ID3D11RenderTargetView **)&view);
            memset(desc, 0, sizeof(desc));
            if (view) ID3D11RenderTargetView_GetDesc((ID3D11RenderTargetView *)view, (void *)desc);
        } else {
            hr = ID3D11Device_CreateUnorderedAccessView(device, (ID3D11Resource *)image, p, (ID3D11UnorderedAccessView **)&view);
            memset(desc, 0, sizeof(desc));
            if (view) ID3D11UnorderedAccessView_GetDesc((ID3D11UnorderedAccessView *)view, (void *)desc);
        }
        printf("view\t%s\t%08lx", rows[i].name, (unsigned long)hr);
        if (view) {
            for (unsigned j=0; j < (rows[i].kind == SRV ? 6u : 5u); ++j) printf("\t%u", desc[j]);
            IUnknown_Release(view);
        }
        puts("");
    }
    ID3D11Texture2D_Release(image);
}

int main(void)
{
    static const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_9_1};
    for (unsigned warp = 0; warp < 2; ++warp)
    for (unsigned l = 0; l < sizeof(levels) / sizeof(*levels); ++l) {
        HRESULT hr = D3D11CreateDevice(NULL, warp ? D3D_DRIVER_TYPE_WARP : D3D_DRIVER_TYPE_HARDWARE,
                                        NULL, 0, &levels[l], 1, D3D11_SDK_VERSION, &device, NULL, NULL);
        printf("device\t%s\t%04x\t%08lx\n", warp ? "warp" : "hw", (unsigned)levels[l], (unsigned long)hr);
        if (FAILED(hr)) continue;
        B("default-no-bind", 0, );
        B("zero-width", 0, d.ByteWidth = 0);
        B("default-cpu-write", 0, d.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE);
        B("immutable-no-init", 0, d.Usage = D3D11_USAGE_IMMUTABLE);
        B("immutable-no-bind", 1, d.Usage = D3D11_USAGE_IMMUTABLE);
        B("dynamic-no-bind", 0, d.Usage = D3D11_USAGE_DYNAMIC; d.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE);
        B("dynamic-no-access", 0, d.Usage = D3D11_USAGE_DYNAMIC; d.BindFlags = D3D11_BIND_VERTEX_BUFFER);
        B("dynamic-index-srv", 0, d.Usage = D3D11_USAGE_DYNAMIC; d.BindFlags = D3D11_BIND_INDEX_BUFFER | D3D11_BIND_SHADER_RESOURCE; d.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE);
        B("staging-no-access", 0, d.Usage = D3D11_USAGE_STAGING);
        B("staging-with-bind", 0, d.Usage = D3D11_USAGE_STAGING; d.BindFlags = D3D11_BIND_VERTEX_BUFFER; d.CPUAccessFlags = D3D11_CPU_ACCESS_READ);
        B("cb-unaligned", 0, d.ByteWidth = 63; d.BindFlags = D3D11_BIND_CONSTANT_BUFFER);
        B("cb-oversize", 0, d.ByteWidth = 65552; d.BindFlags = D3D11_BIND_CONSTANT_BUFFER);
        B("cb-extra-bind", 0, d.BindFlags = D3D11_BIND_CONSTANT_BUFFER | D3D11_BIND_VERTEX_BUFFER);
        B("ignored-stride", 0, d.StructureByteStride = 7);
        B("structured-zero-stride", 0, d.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED; d.BindFlags = D3D11_BIND_SHADER_RESOURCE);
        B("structured-odd-stride", 0, d.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED; d.BindFlags = D3D11_BIND_SHADER_RESOURCE; d.ByteWidth = 63; d.StructureByteStride = 7);
        B("structured-misaligned-width", 0, d.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED; d.BindFlags = D3D11_BIND_SHADER_RESOURCE; d.StructureByteStride = 12);
        B("structured-vertex", 0, d.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED; d.BindFlags = D3D11_BIND_VERTEX_BUFFER | D3D11_BIND_SHADER_RESOURCE; d.StructureByteStride = 16);
        B("raw-no-bind", 0, d.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS);
        B("raw-unaligned", 0, d.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS; d.BindFlags = D3D11_BIND_SHADER_RESOURCE; d.ByteWidth = 63);
        B("raw-structured", 0, d.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS | D3D11_RESOURCE_MISC_BUFFER_STRUCTURED; d.BindFlags = D3D11_BIND_SHADER_RESOURCE; d.StructureByteStride = 16);
        B("rt-uav", 0, d.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_UNORDERED_ACCESS);
        B("depth-bind", 0, d.BindFlags = D3D11_BIND_DEPTH_STENCIL);
        B("indirect-unaligned", 0, d.MiscFlags = D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS; d.ByteWidth = 63);
        B("indirect-small", 0, d.MiscFlags = D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS; d.ByteWidth = 4);
        T("default-no-bind", NULL, );
        T("full-mips", NULL, d.MipLevels = 0);
        T("long-mips", NULL, d.MipLevels = 6);
        T("default-cpu-read", NULL, d.CPUAccessFlags = D3D11_CPU_ACCESS_READ);
        T("dynamic-array", NULL, d.Usage = D3D11_USAGE_DYNAMIC; d.BindFlags = D3D11_BIND_SHADER_RESOURCE; d.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE; d.ArraySize = 2);
        T("dynamic-mips", NULL, d.Usage = D3D11_USAGE_DYNAMIC; d.BindFlags = D3D11_BIND_SHADER_RESOURCE; d.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE; d.MipLevels = 2);
        T("dynamic-no-bind", NULL, d.Usage = D3D11_USAGE_DYNAMIC; d.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE);
        T("staging-no-access", NULL, d.Usage = D3D11_USAGE_STAGING);
        T("staging-msaa", NULL, d.Usage = D3D11_USAGE_STAGING; d.CPUAccessFlags = D3D11_CPU_ACCESS_READ; d.SampleDesc.Count = 4);
        T("msaa-no-bind", NULL, d.SampleDesc.Count = 4);
        T("msaa-srv", NULL, d.SampleDesc.Count = 4; d.BindFlags = D3D11_BIND_SHADER_RESOURCE);
        T("msaa-full-mips", NULL, d.SampleDesc.Count = 4; d.BindFlags = D3D11_BIND_RENDER_TARGET; d.MipLevels = 0);
        T("msaa-init", &initial, d.SampleDesc.Count = 4; d.BindFlags = D3D11_BIND_RENDER_TARGET);
        T("mips-srv-only", NULL, d.BindFlags = D3D11_BIND_SHADER_RESOURCE; d.MiscFlags = D3D11_RESOURCE_MISC_GENERATE_MIPS; d.MipLevels = 0);
        T("mips-single", NULL, d.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET; d.MiscFlags = D3D11_RESOURCE_MISC_GENERATE_MIPS);
        T("cube-non-square", NULL, d.MiscFlags = D3D11_RESOURCE_MISC_TEXTURECUBE; d.ArraySize = 6; d.Height = 8; d.BindFlags = D3D11_BIND_SHADER_RESOURCE);
        T("cube-plain-array", NULL, d.MiscFlags = D3D11_RESOURCE_MISC_TEXTURECUBE; d.ArraySize = 7; d.BindFlags = D3D11_BIND_SHADER_RESOURCE);
        T("cube-no-bind", NULL, d.MiscFlags = D3D11_RESOURCE_MISC_TEXTURECUBE; d.ArraySize = 6);
        T("bc-unaligned", NULL, d.Format = DXGI_FORMAT_BC1_UNORM; d.Width = 5; d.BindFlags = D3D11_BIND_SHADER_RESOURCE);
        T("bc-small", NULL, d.Format = DXGI_FORMAT_BC1_UNORM; d.Width = d.Height = 2; d.BindFlags = D3D11_BIND_SHADER_RESOURCE);
        T("gdi-rgba", NULL, d.MiscFlags = D3D11_RESOURCE_MISC_GDI_COMPATIBLE; d.BindFlags = D3D11_BIND_RENDER_TARGET);
        T("gdi-bgra", NULL, d.MiscFlags = D3D11_RESOURCE_MISC_GDI_COMPATIBLE; d.BindFlags = D3D11_BIND_RENDER_TARGET; d.Format = DXGI_FORMAT_B8G8R8A8_UNORM);
        T("gdi-bgra-mips", NULL, d.MiscFlags = D3D11_RESOURCE_MISC_GDI_COMPATIBLE; d.BindFlags = D3D11_BIND_RENDER_TARGET; d.Format = DXGI_FORMAT_B8G8R8A8_UNORM; d.MipLevels = 2);
        views();
        ID3D11Device_Release(device);
    }
}
