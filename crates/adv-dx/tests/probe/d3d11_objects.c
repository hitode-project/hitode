/* Windows hardware and WARP oracle.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o d3d11_objects.exe d3d11_objects.c -ld3d11 -ldxguid
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <initguid.h>
#include <d3d11_1.h>
#include <dxgi1_2.h>
#include <stdio.h>
#include <string.h>
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

static void shared_probe(D3D_DRIVER_TYPE driver)
{
    ID3D11Device *first = NULL, *second = NULL;
    D3D11CreateDevice(NULL, driver, NULL, 0, NULL, 0, 7, &first, NULL, NULL);
    D3D11CreateDevice(NULL, driver, NULL, 0, NULL, 0, 7, &second, NULL, NULL);
    if (!first || !second) return;
    IDXGIDevice *dxgi = NULL;
    IDXGIAdapter *adapter = NULL;
    IDXGIFactory2 *factory = NULL;
    DXGI_ADAPTER_DESC adapter_desc = {0};
    if (SUCCEEDED(ID3D11Device_QueryInterface(first, &IID_IDXGIDevice, (void **)&dxgi))) {
        if (SUCCEEDED(IDXGIDevice_GetAdapter(dxgi, &adapter))) {
            IDXGIAdapter_GetDesc(adapter, &adapter_desc);
            IDXGIAdapter_GetParent(adapter, &IID_IDXGIFactory2, (void **)&factory);
            IDXGIAdapter_Release(adapter);
        }
        IDXGIDevice_Release(dxgi);
    }
    const UINT flags[] = {0, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
                          D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX};
    for (unsigned i = 0; i < sizeof(flags) / sizeof(*flags); i++) {
        D3D11_TEXTURE2D_DESC desc = {16, 16, 1, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0}, 0,
                                    D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, flags[i]};
        ID3D11Texture2D *texture = NULL, *alias = NULL, *same = NULL;
        HRESULT create = ID3D11Device_CreateTexture2D(first, &desc, NULL, &texture);
        printf("share-create\t%x\t%08lx\n", flags[i], (unsigned long)create);
        if (!texture) continue;
        IDXGIResource *resource = NULL;
        HRESULT hr = ID3D11Texture2D_QueryInterface(texture, &IID_IDXGIResource, (void **)&resource);
        printf("share-qi-resource\t%x\t%08lx\n", flags[i], (unsigned long)hr);
        if (!resource) {
            ID3D11Texture2D_Release(texture);
            continue;
        }
        HANDLE handle = (HANDLE)(uintptr_t)0x1234;
        hr = IDXGIResource_GetSharedHandle(resource, &handle);
        printf("share-get\t%x\t%08lx\t%u\t%u\n", flags[i], (unsigned long)hr, handle == NULL,
               handle == (HANDLE)(uintptr_t)0x1234);
        if (SUCCEEDED(hr)) {
            if (factory) {
                LUID luid = {0};
                HRESULT status = IDXGIFactory2_GetSharedResourceAdapterLuid(factory, handle, &luid);
                printf("share-adapter\t%x\t%08lx\t%u\n", flags[i], (unsigned long)status,
                       luid.LowPart == adapter_desc.AdapterLuid.LowPart
                           && luid.HighPart == adapter_desc.AdapterLuid.HighPart);
            }
            hr = ID3D11Device_OpenSharedResource(first, handle, &IID_ID3D11Texture2D, (void **)&same);
            printf("share-open-same\t%x\t%08lx\t%u\n", flags[i], (unsigned long)hr, same == texture);
            hr = ID3D11Device_OpenSharedResource(second, handle, &IID_ID3D11Texture2D, (void **)&alias);
            printf("share-open-other\t%x\t%08lx\t%u\n", flags[i], (unsigned long)hr, alias == texture);
            void *unsupported = (void *)(uintptr_t)0x1234;
            hr = ID3D11Device_OpenSharedResource(second, handle, &key, &unsupported);
            printf("share-open-iid\t%x\t%08lx\t%u\n", flags[i], (unsigned long)hr, unsupported == NULL);
        }
        IDXGIKeyedMutex *mutex = NULL;
        hr = ID3D11Texture2D_QueryInterface(texture, &IID_IDXGIKeyedMutex, (void **)&mutex);
        printf("share-qi-keyed\t%x\t%08lx\n", flags[i], (unsigned long)hr);
        if (mutex) {
            HRESULT wrong = IDXGIKeyedMutex_AcquireSync(mutex, 1, 0);
            HRESULT acquire = IDXGIKeyedMutex_AcquireSync(mutex, 0, 0);
            HRESULT release = IDXGIKeyedMutex_ReleaseSync(mutex, 1);
            printf("share-keyed\t%x\t%08lx\t%08lx\t%08lx\n", flags[i], (unsigned long)wrong,
                   (unsigned long)acquire, (unsigned long)release);
            IDXGIKeyedMutex_Release(mutex);
        }
        IDXGIResource1 *resource1 = NULL;
        hr = ID3D11Texture2D_QueryInterface(texture, &IID_IDXGIResource1, (void **)&resource1);
        if (resource1) {
            HANDLE nt_handle = NULL;
            const WCHAR name[] = L"Local\\HitodeD3D11Probe";
            hr = IDXGIResource1_CreateSharedHandle(resource1, NULL, DXGI_SHARED_RESOURCE_READ, name, &nt_handle);
            printf("share-nt-create\t%x\t%08lx\t%u\n", flags[i], (unsigned long)hr, nt_handle != NULL);
            if (nt_handle) {
                ID3D11Device1 *device1 = NULL;
                if (SUCCEEDED(ID3D11Device_QueryInterface(second, &IID_ID3D11Device1, (void **)&device1))) {
                    ID3D11Texture2D *nt_alias = NULL;
                    hr = ID3D11Device1_OpenSharedResource1(device1, nt_handle, &IID_ID3D11Texture2D, (void **)&nt_alias);
                    printf("share-nt-open\t%x\t%08lx\n", flags[i], (unsigned long)hr);
                    if (nt_alias) ID3D11Texture2D_Release(nt_alias);
                    nt_alias = NULL;
                    hr = ID3D11Device1_OpenSharedResourceByName(device1, name, DXGI_SHARED_RESOURCE_READ,
                                                              &IID_ID3D11Texture2D, (void **)&nt_alias);
                    printf("share-nt-name\t%x\t%08lx\n", flags[i], (unsigned long)hr);
                    if (nt_alias) ID3D11Texture2D_Release(nt_alias);
                    ID3D11Device1_Release(device1);
                }
                CloseHandle(nt_handle);
            }
            IDXGIResource1_Release(resource1);
        }
        if (same) ID3D11Texture2D_Release(same);
        if (alias) ID3D11Texture2D_Release(alias);
        IDXGIResource_Release(resource);
        ID3D11Texture2D_Release(texture);
        if (flags[i] == D3D11_RESOURCE_MISC_SHARED && handle && handle != (HANDLE)(uintptr_t)0x1234) {
            alias = NULL;
            hr = ID3D11Device_OpenSharedResource(second, handle, &IID_ID3D11Texture2D, (void **)&alias);
            printf("share-open-released\t%08lx\t%u\n", (unsigned long)hr, alias == NULL);
            if (alias) ID3D11Texture2D_Release(alias);
        }
    }
    if (factory) {
        LUID luid = {0x1234, 0x5678};
        HRESULT status = IDXGIFactory2_GetSharedResourceAdapterLuid(factory, (HANDLE)(uintptr_t)0x1234, &luid);
        printf("share-adapter-invalid\t%08lx\t%u\n", (unsigned long)status,
               luid.LowPart == 0x1234 && luid.HighPart == 0x5678);
        IDXGIFactory2_Release(factory);
    }
    ID3D11Device_Release(first);
    ID3D11Device_Release(second);
}

static void resource_probe(D3D_DRIVER_TYPE driver)
{
    ID3D11Device *device = NULL;
    ID3D11DeviceContext *context = NULL;
    D3D11CreateDevice(NULL, driver, NULL, 0, NULL, 0, 7, &device, NULL, &context);
    if (!device) return;
    const DWORD initial[] = {0x01020304, 0x05060708};
    const D3D11_SUBRESOURCE_DATA data = {initial, 0, 0};
    D3D11_BUFFER_DESC buffer_desc = {sizeof(initial), D3D11_USAGE_DEFAULT,
                                     D3D11_BIND_UNORDERED_ACCESS, 0, 0, 0};
    ID3D11Buffer *buffer = NULL, *readback = NULL;
    if (SUCCEEDED(ID3D11Device_CreateBuffer(device, &buffer_desc, &data, &buffer))) {
        buffer_desc.Usage = D3D11_USAGE_STAGING;
        buffer_desc.BindFlags = 0;
        buffer_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        ID3D11Device_CreateBuffer(device, &buffer_desc, NULL, &readback);
        const DXGI_FORMAT formats[] = {DXGI_FORMAT_R8G8_UINT, DXGI_FORMAT_R32_FLOAT};
        for (unsigned i = 0; i < sizeof(formats)/sizeof(*formats); ++i) {
            D3D11_UNORDERED_ACCESS_VIEW_DESC view_desc = {
                .Format = formats[i], .ViewDimension = D3D11_UAV_DIMENSION_BUFFER, .Buffer = {1, 1, 0},
            };
            ID3D11UnorderedAccessView *uav = NULL;
            HRESULT hr = ID3D11Device_CreateUnorderedAccessView(device, (ID3D11Resource *)buffer, &view_desc, &uav);
            printf("typed-uav-create\t%u\t%08lx\n", (unsigned)formats[i], (unsigned long)hr);
            if (!uav || !readback) {
                if (uav) ID3D11UnorderedAccessView_Release(uav);
                continue;
            }
            ID3D11DeviceContext_UpdateSubresource(context, (ID3D11Resource *)buffer, 0, NULL, initial, 0, 0);
            if (i == 0) {
                const UINT clear[] = {0x12a, 0x134, 0, 0};
                ID3D11DeviceContext_ClearUnorderedAccessViewUint(context, uav, clear);
            } else {
                const UINT clear[] = {0x7fc01234, 0, 0, 0};
                FLOAT words[4];
                memcpy(words, clear, sizeof(words));
                ID3D11DeviceContext_ClearUnorderedAccessViewFloat(context, uav, words);
            }
            ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)readback, (ID3D11Resource *)buffer);
            D3D11_MAPPED_SUBRESOURCE map;
            hr = ID3D11DeviceContext_Map(context, (ID3D11Resource *)readback, 0, D3D11_MAP_READ, 0, &map);
            if (SUCCEEDED(hr)) {
                const DWORD *pixels = map.pData;
                printf("typed-uav-data\t%u\t%08lx\t%08lx\n", (unsigned)formats[i],
                       (unsigned long)pixels[0], (unsigned long)pixels[1]);
                ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)readback, 0);
            }
            ID3D11UnorderedAccessView_Release(uav);
        }
        if (readback) ID3D11Buffer_Release(readback);
        ID3D11Buffer_Release(buffer);
    }
    D3D11_TEXTURE2D_DESC desc = {4, 4, 3, 1, DXGI_FORMAT_R8G8B8A8_UNORM, {1, 0}, 0,
                               D3D11_BIND_SHADER_RESOURCE, 0, D3D11_RESOURCE_MISC_RESOURCE_CLAMP};
    ID3D11Texture2D *texture = NULL;
    if (SUCCEEDED(ID3D11Device_CreateTexture2D(device, &desc, NULL, &texture))) {
        const float minimum[] = {1.5f, -0.5f, 2.25f, 20.0f};
        for (unsigned i = 0; i < sizeof(minimum)/sizeof(*minimum); ++i) {
            ID3D11DeviceContext_SetResourceMinLOD(context, (ID3D11Resource *)texture, minimum[i]);
            float actual = ID3D11DeviceContext_GetResourceMinLOD(context, (ID3D11Resource *)texture);
            printf("resource-min-lod\t%.9g\t%.9g\n", minimum[i], actual);
        }
        ID3D11Texture2D_Release(texture);
    }
    desc = (D3D11_TEXTURE2D_DESC){4, 4, 1, 1, DXGI_FORMAT_R24G8_TYPELESS, {1, 0}, 0,
                                D3D11_BIND_DEPTH_STENCIL | D3D11_BIND_SHADER_RESOURCE, 0, 0};
    texture = NULL;
    HRESULT hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &texture);
    printf("d24-create\t%08lx\n", (unsigned long)hr);
    if (texture) {
        D3D11_DEPTH_STENCIL_VIEW_DESC vd = {DXGI_FORMAT_D24_UNORM_S8_UINT, D3D11_DSV_DIMENSION_TEXTURE2D, 0, {{0}}};
        ID3D11DepthStencilView *dsv = NULL;
        hr = ID3D11Device_CreateDepthStencilView(device, (ID3D11Resource *)texture, &vd, &dsv);
        printf("d24-view\t%08lx\n", (unsigned long)hr);
        if (dsv) ID3D11DeviceContext_ClearDepthStencilView(context, dsv, 3, 1.f / 3.f, 0x71);
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE;
        ID3D11Texture2D *staging = NULL;
        hr = ID3D11Device_CreateTexture2D(device, &desc, NULL, &staging);
        printf("d24-staging\t%08lx\n", (unsigned long)hr);
        if (staging) {
            ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)staging, (ID3D11Resource *)texture);
            D3D11_MAPPED_SUBRESOURCE map;
            hr = ID3D11DeviceContext_Map(context, (ID3D11Resource *)staging, 0, D3D11_MAP_READ_WRITE, 0, &map);
            printf("d24-map\t%08lx\t%08lx\n", (unsigned long)hr,
                   SUCCEEDED(hr) ? (unsigned long)*(DWORD *)map.pData : 0ul);
            if (SUCCEEDED(hr)) {
                *(DWORD *)map.pData = 0x35800000;
                ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)staging, 0);
                ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)texture, (ID3D11Resource *)staging);
                ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)staging, (ID3D11Resource *)texture);
                hr = ID3D11DeviceContext_Map(context, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &map);
                printf("d24-roundtrip\t%08lx\t%08lx\n", (unsigned long)hr,
                       SUCCEEDED(hr) ? (unsigned long)*(DWORD *)map.pData : 0ul);
                if (SUCCEEDED(hr)) ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)staging, 0);
            }
            ID3D11Texture2D_Release(staging);
        }
        if (dsv) ID3D11DepthStencilView_Release(dsv);
        ID3D11Texture2D_Release(texture);
    }
    ID3D11DeviceContext_Release(context);
    ID3D11Device_Release(device);
}

static void fullscreen_probe(void)
{
    const WCHAR name[] = L"HitodeDXGIProbe";
    HINSTANCE instance = GetModuleHandleW(NULL);
    WNDCLASSW wc = {.lpfnWndProc = DefWindowProcW, .hInstance = instance, .lpszClassName = name};
    if (!RegisterClassW(&wc)) return;
    HWND hwnd = CreateWindowW(name, name, WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                              CW_USEDEFAULT, CW_USEDEFAULT, 640, 480, NULL, NULL, instance, NULL);
    if (!hwnd) return;
    SetForegroundWindow(hwnd);
    DXGI_SWAP_CHAIN_DESC desc = {
        .BufferDesc = {.Width = 640, .Height = 480, .Format = DXGI_FORMAT_R8G8B8A8_UNORM},
        .SampleDesc = {1, 0}, .BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT,
        .BufferCount = 1, .OutputWindow = hwnd, .Windowed = TRUE,
    };
    IDXGISwapChain *chain = NULL;
    HRESULT hr = D3D11CreateDeviceAndSwapChain(NULL, D3D_DRIVER_TYPE_HARDWARE, NULL, 0, NULL, 0,
                                              7, &desc, &chain, NULL, NULL, NULL);
    printf("fullscreen-create\t%08lx\n", (unsigned long)hr);
    if (chain) {
        IDXGIFactory *factory = NULL;
        hr = IDXGISwapChain_GetParent(chain, &IID_IDXGIFactory, (void **)&factory);
        printf("fullscreen-parent\t%08lx\n", (unsigned long)hr);
        if (factory) {
            HRESULT bad = IDXGIFactory_MakeWindowAssociation(factory, NULL, DXGI_MWA_NO_ALT_ENTER);
            printf("association-null-with-flags\t%08lx\n", (unsigned long)bad);
            const UINT flags[] = {0, DXGI_MWA_NO_ALT_ENTER, DXGI_MWA_NO_WINDOW_CHANGES, 0};
            for (unsigned i = 0; i < sizeof(flags)/sizeof(*flags); i++) {
                HWND associate = i == 3 ? NULL : hwnd;
                HRESULT association = IDXGIFactory_MakeWindowAssociation(factory, associate, flags[i]);
                INPUT keys[4] = {
                    {.type = INPUT_KEYBOARD, .ki = {.wVk = VK_MENU}},
                    {.type = INPUT_KEYBOARD, .ki = {.wVk = VK_RETURN}},
                    {.type = INPUT_KEYBOARD, .ki = {.wVk = VK_RETURN, .dwFlags = KEYEVENTF_KEYUP}},
                    {.type = INPUT_KEYBOARD, .ki = {.wVk = VK_MENU, .dwFlags = KEYEVENTF_KEYUP}},
                };
                UINT sent = SendInput(4, keys, sizeof(INPUT));
                for (unsigned pulse = 0; pulse < 20; pulse++) {
                    MSG message;
                    while (PeekMessageW(&message, NULL, 0, 0, PM_REMOVE)) {
                        TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    Sleep(10);
                }
                BOOL fullscreen = FALSE;
                hr = IDXGISwapChain_GetFullscreenState(chain, &fullscreen, NULL);
                printf("association-alt-enter\t%u\t%u\t%08lx\t%u\t%08lx\t%d\n",
                       associate != NULL, flags[i], (unsigned long)association, sent,
                       (unsigned long)hr, fullscreen);
                if (fullscreen) IDXGISwapChain_SetFullscreenState(chain, FALSE, NULL);
            }
            IDXGIFactory_Release(factory);
        }
        IDXGISwapChain_Release(chain);
    }
    DestroyWindow(hwnd);
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc > 1 && !strcmp(argv[1], "--fullscreen")) {
        fullscreen_probe();
        return 0;
    }
    if (argc > 1 && (!strcmp(argv[1], "--shared") || !strcmp(argv[1], "--resource"))) {
        void (*supplement)(D3D_DRIVER_TYPE) = !strcmp(argv[1], "--shared") ? shared_probe : resource_probe;
        puts("driver\thw");
        supplement(D3D_DRIVER_TYPE_HARDWARE);
        puts("driver\twarp");
        supplement(D3D_DRIVER_TYPE_WARP);
        return 0;
    }
    puts("driver\thw");
    probe(D3D_DRIVER_TYPE_HARDWARE);
    puts("driver\twarp");
    probe(D3D_DRIVER_TYPE_WARP);
}
