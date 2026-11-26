/* Windows hardware and WARP oracle.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o d3d11_objects.exe d3d11_objects.c -ld3d11
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <initguid.h>
#include <stdio.h>
DEFINE_GUID(key, 0x74387274, 0xb057, 0x5577, 0x8e, 0x81, 0x57, 0x18, 0xd5, 0xfc, 0x5b, 0x22);

static unsigned refs(IUnknown *object)
{
    IUnknown_AddRef(object);
    return IUnknown_Release(object);
}
#define REFS(x) refs((IUnknown *)(x))
static void probe(D3D_DRIVER_TYPE driver)
{
    ID3D11Device *device = NULL;
    ID3D11DeviceContext *context = NULL;
    ID3D11Texture2D *texture = NULL;
    ID3D11RenderTargetView *view = NULL;
    D3D11_TEXTURE2D_DESC desc = {16, 16, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0}, 0, D3D11_BIND_RENDER_TARGET, 0, 0};
    HRESULT hr = D3D11CreateDevice(NULL, driver, NULL, 0, NULL, 0, 7, &device, NULL, NULL);
    printf("create\t%08lx\t%u\n", (unsigned long)hr, REFS(device));
    if (!device) return;
    ID3D11Device_GetImmediateContext(device, &context);
    printf("context\t%u\t%u\n", REFS(device), REFS(context));
    ID3D11Device_CreateTexture2D(device, &desc, NULL, &texture);
    printf("texture\t%u\t%u\t%08x\n", REFS(device), REFS(texture), ID3D11Texture2D_GetEvictionPriority(texture));
    ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)texture, NULL, &view);
    printf("view\t%u\t%u\t%u\n", REFS(device), REFS(texture), REFS(view));
    ID3D11DeviceContext_OMSetRenderTargets(context, 1, &view, NULL);
    printf("bound\t%u\t%u\t%u\n", REFS(device), REFS(texture), REFS(view));
    printf("release-view\t%lu\n", ID3D11RenderTargetView_Release(view));
    printf("bound-released\t%u\t%u\n", REFS(device), REFS(texture));
    ID3D11DeviceContext_OMGetRenderTargets(context, 1, &view, NULL);
    printf("get-view\t%u\t%u\n", REFS(device), REFS(view));
    printf("release-texture\t%lu\n", ID3D11Texture2D_Release(texture));
    printf("texture-released\t%u\n", REFS(device));
    ID3D11RenderTargetView_GetResource(view, (ID3D11Resource **)&texture);
    printf("get-resource\t%u\t%u\n", REFS(device), REFS(texture));

    unsigned len = 99, small[2] = {0x12345678, 0x12345678};
    hr = ID3D11Device_GetPrivateData(device, &key, &len, small);
    printf("get-missing\t%08lx\t%u\t%08x\n", (unsigned long)hr, len, small[0]);
    hr = ID3D11Device_SetPrivateData(device, &key, 0, NULL);
    printf("remove-missing\t%08lx\n", (unsigned long)hr);
    hr = ID3D11Device_SetPrivateDataInterface(device, &key, (IUnknown *)texture);
    printf("set-interface\t%08lx\t%u\n", (unsigned long)hr, REFS(texture));
    len = 1;
    hr = ID3D11Device_GetPrivateData(device, &key, &len, small);
    printf("small-interface\t%08lx\t%u\t%08x\t%u\n", (unsigned long)hr, len, small[0], REFS(texture));
    len = 0;
    hr = ID3D11Device_GetPrivateData(device, &key, &len, NULL);
    printf("size-interface\t%08lx\t%u\t%u\n", (unsigned long)hr, len, REFS(texture));
    len = 8;
    hr = ID3D11Device_GetPrivateData(device, &key, &len, small);
    printf("get-interface\t%08lx\t%u\t%u\t%u\n", (unsigned long)hr, len, small[0] == (unsigned)(uintptr_t)texture, REFS(texture));
    if (SUCCEEDED(hr)) IUnknown_Release((IUnknown *)(uintptr_t)small[0]);
    hr = ID3D11Device_SetPrivateDataInterface(device, &key, NULL);
    printf("null-interface\t%08lx\t%u\n", (unsigned long)hr, REFS(texture));
    len = 10;
    hr = ID3D11Device_GetPrivateData(device, &key, &len, small);
    printf("get-after-null\t%08lx\t%u\t%u\n", (unsigned long)hr, len, small[0] == 0);
    ID3D11Texture2D_Release(texture);
    ID3D11RenderTargetView_Release(view);
    ID3D11DeviceContext_Release(context);
    printf("device-release\t%lu\n", ID3D11Device_Release(device));
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("driver\thw");
    probe(D3D_DRIVER_TYPE_HARDWARE);
    puts("driver\twarp");
    probe(D3D_DRIVER_TYPE_WARP);
}
