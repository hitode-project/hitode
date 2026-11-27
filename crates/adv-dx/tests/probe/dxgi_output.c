/* Supplemental Windows hardware and WARP output rows.
 * Build: i686-w64-mingw32-gcc -O2 -Wall -Wextra -Werror -o dxgi_output.exe dxgi_output.c -ld3d11 -ldxgi -lgdi32 -luser32 -luuid
 */
#define COBJMACROS
#include <windows.h>
#include <initguid.h>
#include <d3d11.h>
#include <dxgi1_5.h>
#include <stdio.h>

static void result(const char *name, HRESULT hr)
{
    printf("%s\t%08lx\n", name, (unsigned long)hr);
}

static void gamma_rows(const char *state, IDXGIOutput *output, BOOL program)
{
    DXGI_GAMMA_CONTROL_CAPABILITIES caps = {0};
    DXGI_GAMMA_CONTROL gamma = {0};
    HRESULT hr = IDXGIOutput_GetGammaControlCapabilities(output, &caps);
    printf("gamma-caps-%s\t%08lx\t%d\t%g\t%g\t%u\n", state, (unsigned long)hr,
           (int)caps.ScaleAndOffsetSupported, caps.MinConvertedValue, caps.MaxConvertedValue,
           caps.NumGammaControlPoints);
    if (SUCCEEDED(hr)) {
        UINT count = caps.NumGammaControlPoints < 1025 ? caps.NumGammaControlPoints : 1025;
        for (UINT i = 0; i < count; ++i)
            printf("gamma-point-%u\t%.9g\n", i, caps.ControlPointPositions[i]);
    }
    hr = IDXGIOutput_GetGammaControl(output, &gamma);
    printf("gamma-get-%s\t%08lx\n", state, (unsigned long)hr);
    if (program) {
        gamma.Scale.Red = gamma.Scale.Green = gamma.Scale.Blue = 1;
        for (UINT i = 0; i < 1025; ++i)
            gamma.GammaCurve[i].Red = gamma.GammaCurve[i].Green = gamma.GammaCurve[i].Blue = i / 1024.0f;
        result("gamma-set-identity", IDXGIOutput_SetGammaControl(output, &gamma));
        ZeroMemory(&gamma, sizeof(gamma));
        hr = IDXGIOutput_GetGammaControl(output, &gamma);
        printf("gamma-after-set\t%08lx\t%.9g\t%.9g\t%.9g\n", (unsigned long)hr,
               gamma.Scale.Red, gamma.Offset.Green, gamma.GammaCurve[512].Blue);
    }
}

static void duplication_rows(IDXGIOutputDuplication *duplicate, ID3D11Device *device,
                             ID3D11DeviceContext *context)
{
    DXGI_OUTDUPL_DESC desc;
    DXGI_OUTDUPL_FRAME_INFO info = {0};
    IDXGIResource *resource = NULL;
    IDXGISurface *surface = NULL;
    DXGI_MAPPED_RECT map = {0};
    UINT bytes = 0;
    IDXGIOutputDuplication_GetDesc(duplicate, &desc);
    printf("duplication-desc\t%u\t%u\t%u\t%u\t%u\t%d\n", desc.ModeDesc.Width,
           desc.ModeDesc.Height, desc.ModeDesc.Format, desc.ModeDesc.RefreshRate.Numerator,
           desc.ModeDesc.RefreshRate.Denominator, desc.Rotation);
    result("release-without-frame", IDXGIOutputDuplication_ReleaseFrame(duplicate));
    result("acquire-null-info", IDXGIOutputDuplication_AcquireNextFrame(duplicate, 0, NULL, &resource));
    HRESULT hr = IDXGIOutputDuplication_AcquireNextFrame(duplicate, 500, &info, &resource);
    printf("acquire\t%08lx\t%u\t%u\t%u\t%d\n", (unsigned long)hr,
           info.AccumulatedFrames, info.TotalMetadataBufferSize, info.PointerShapeBufferSize,
           (int)info.PointerPosition.Visible);
    if (FAILED(hr)) return;
    result("dirty-size", IDXGIOutputDuplication_GetFrameDirtyRects(duplicate, 0, NULL, &bytes));
    printf("dirty-bytes\t%u\n", bytes);
    result("move-size", IDXGIOutputDuplication_GetFrameMoveRects(duplicate, 0, NULL, &bytes));
    printf("move-bytes\t%u\n", bytes);
    DXGI_OUTDUPL_POINTER_SHAPE_INFO shape = {0};
    result("pointer-size", IDXGIOutputDuplication_GetFramePointerShape(duplicate, 0, NULL, &bytes, &shape));
    printf("pointer-bytes\t%u\n", bytes);
    result("map-desktop", IDXGIOutputDuplication_MapDesktopSurface(duplicate, &map));
    result("unmap-desktop", IDXGIOutputDuplication_UnMapDesktopSurface(duplicate));
    IDXGIResource *second = NULL;
    result("acquire-while-held", IDXGIOutputDuplication_AcquireNextFrame(duplicate, 0, &info, &second));
    if (second) IDXGIResource_Release(second);
    result("duplicate-resource-surface", IDXGIResource_QueryInterface(resource, &IID_IDXGISurface, (void **)&surface));
    if (surface) {
        DXGI_SURFACE_DESC surface_desc;
        IDXGISurface_GetDesc(surface, &surface_desc);
        printf("duplicate-resource-desc\t%u\t%u\t%u\n", surface_desc.Width,
               surface_desc.Height, surface_desc.Format);
        IDXGISurface_Release(surface);
    }
    ID3D11Texture2D *image = NULL, *staging = NULL;
    if (SUCCEEDED(IDXGIResource_QueryInterface(resource, &IID_ID3D11Texture2D, (void **)&image))) {
        D3D11_TEXTURE2D_DESC image_desc;
        ID3D11Texture2D_GetDesc(image, &image_desc);
        image_desc.Usage = D3D11_USAGE_STAGING;
        image_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        image_desc.BindFlags = image_desc.MiscFlags = 0;
        if (SUCCEEDED(ID3D11Device_CreateTexture2D(device, &image_desc, NULL, &staging))) {
            ID3D11DeviceContext_CopyResource(context, (ID3D11Resource *)staging, (ID3D11Resource *)image);
            D3D11_MAPPED_SUBRESOURCE mapped;
            if (SUCCEEDED(ID3D11DeviceContext_Map(context, (ID3D11Resource *)staging, 0, D3D11_MAP_READ, 0, &mapped))) {
                printf("duplicate-first-texel\t%08lx\n", (unsigned long)*(const DWORD *)mapped.pData);
                ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)staging, 0);
            }
            ID3D11Texture2D_Release(staging);
        }
        ID3D11Texture2D_Release(image);
    }
    result("release-frame", IDXGIOutputDuplication_ReleaseFrame(duplicate));
    IDXGIResource_Release(resource);
    resource = NULL;
    result("acquire-after-release-zero", IDXGIOutputDuplication_AcquireNextFrame(duplicate, 0, &info, &resource));
    if (resource) {
        IDXGIOutputDuplication_ReleaseFrame(duplicate);
        IDXGIResource_Release(resource);
    }
}

static void display_rows(IDXGIOutput *output, IDXGIOutput1 *output1, IDXGISwapChain1 *chain,
                         ID3D11Device *device, ID3D11DeviceContext *context)
{
    HRESULT hr;
    IDXGISurface *scanout = NULL, *surface = NULL;
    IDXGIResource *resource = NULL;
    ID3D11Texture2D *staging = NULL, *back = NULL;
    ID3D11RenderTargetView *rtv = NULL;
    D3D11_TEXTURE2D_DESC desc = {0};
    desc.Width = 32;
    desc.Height = 24;
    desc.ArraySize = desc.MipLevels = desc.SampleDesc.Count = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.Usage = D3D11_USAGE_STAGING;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE;
    if (FAILED(ID3D11Device_CreateTexture2D(device, &desc, NULL, &staging))) return;
    ID3D11Texture2D_QueryInterface(staging, &IID_IDXGISurface, (void **)&surface);
    ID3D11Texture2D_QueryInterface(staging, &IID_IDXGIResource, (void **)&resource);
    result("display-copy-windowed", IDXGIOutput_GetDisplaySurfaceData(output, surface));
    if (output1) result("display-copy1-windowed", IDXGIOutput1_GetDisplaySurfaceData1(output1, resource));
    IDXGIOutputDuplication *before_fullscreen = NULL;
    if (output1)
        result("duplicate-before-fullscreen", IDXGIOutput1_DuplicateOutput(output1, (IUnknown *)device,
                                                                           &before_fullscreen));
    hr = IDXGISwapChain1_SetFullscreenState(chain, TRUE, output);
    result("enter-fullscreen", hr);
    if (SUCCEEDED(hr)) {
        if (before_fullscreen) {
            DXGI_OUTDUPL_FRAME_INFO info;
            IDXGIResource *frame = NULL;
            result("acquire-after-fullscreen-transition",
                   IDXGIOutputDuplication_AcquireNextFrame(before_fullscreen, 0, &info, &frame));
            if (frame) {
                IDXGIOutputDuplication_ReleaseFrame(before_fullscreen);
                IDXGIResource_Release(frame);
            }
        }
        gamma_rows("fullscreen", output, TRUE);
        if (SUCCEEDED(IDXGISwapChain1_GetBuffer(chain, 0, &IID_ID3D11Texture2D, (void **)&back))
            && SUCCEEDED(ID3D11Device_CreateRenderTargetView(device, (ID3D11Resource *)back, NULL, &rtv))) {
            const FLOAT color[4] = {1.0f, 0.25f, 0.0f, 1.0f};
            ID3D11DeviceContext_ClearRenderTargetView(context, rtv, color);
            result("fullscreen-present", IDXGISwapChain1_Present(chain, 1, 0));
            ID3D11RenderTargetView_Release(rtv);
        }
        result("display-set-non-backbuffer", IDXGIOutput_SetDisplaySurface(output, surface));
        if (back && SUCCEEDED(ID3D11Texture2D_QueryInterface(back, &IID_IDXGISurface, (void **)&scanout))) {
            result("display-set-backbuffer", IDXGIOutput_SetDisplaySurface(output, scanout));
            IDXGISurface_Release(scanout);
        }
        hr = IDXGIOutput_GetDisplaySurfaceData(output, surface);
        result("display-copy-fullscreen", hr);
        if (output1) result("display-copy1-fullscreen", IDXGIOutput1_GetDisplaySurfaceData1(output1, resource));
        D3D11_MAPPED_SUBRESOURCE mapped;
        if (SUCCEEDED(hr) && SUCCEEDED(ID3D11DeviceContext_Map(context, (ID3D11Resource *)staging, 0,
                                                             D3D11_MAP_READ, 0, &mapped))) {
            printf("display-first-texel\t%08lx\n", (unsigned long)*(const DWORD *)mapped.pData);
            ID3D11DeviceContext_Unmap(context, (ID3D11Resource *)staging, 0);
        }
        if (back) ID3D11Texture2D_Release(back);
        result("leave-fullscreen", IDXGISwapChain1_SetFullscreenState(chain, FALSE, NULL));
        gamma_rows("post-fullscreen", output, FALSE);
    }
    if (before_fullscreen) IDXGIOutputDuplication_Release(before_fullscreen);
    if (resource) IDXGIResource_Release(resource);
    if (surface) IDXGISurface_Release(surface);
    ID3D11Texture2D_Release(staging);
}

int main(void)
{
    HWND window = CreateWindowA("STATIC", "DXGI output probe", WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                                20, 20, 640, 480, NULL, NULL, GetModuleHandleA(NULL), NULL);
    for (UINT warp = 0; warp <= 1; ++warp) {
        ID3D11Device *device = NULL;
        ID3D11DeviceContext *context = NULL;
        IDXGIFactory2 *factory = NULL;
        IDXGIAdapter *adapter = NULL;
        IDXGIOutput *output = NULL;
        IDXGIOutput1 *output1 = NULL;
        IDXGIOutput5 *output5 = NULL;
        IDXGISwapChain1 *chain = NULL;
        IDXGIOutputDuplication *duplicate = NULL, *second = NULL;
        printf("backend\t%s\n", warp ? "warp" : "hardware");
        HRESULT hr = D3D11CreateDevice(NULL, warp ? D3D_DRIVER_TYPE_WARP : D3D_DRIVER_TYPE_HARDWARE,
                                      NULL, 0, NULL, 0, D3D11_SDK_VERSION, &device, NULL, &context);
        result("create-device", hr);
        if (FAILED(hr)) continue;
        hr = CreateDXGIFactory1(&IID_IDXGIFactory2, (void **)&factory);
        result("create-factory2", hr);
        if (FAILED(hr)) goto done;
        DXGI_SWAP_CHAIN_DESC1 desc = {0};
        desc.Width = 64;
        desc.Height = 48;
        desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        desc.SampleDesc.Count = 1;
        desc.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
        desc.BufferCount = 2;
        desc.SwapEffect = DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL;
        desc.Scaling = DXGI_SCALING_STRETCH;
        desc.AlphaMode = DXGI_ALPHA_MODE_IGNORE;
        result("corewindow-null", IDXGIFactory2_CreateSwapChainForCoreWindow(factory, (IUnknown *)device,
                                                                              NULL, &desc, NULL, &chain));
        if (chain) IDXGISwapChain1_Release(chain);
        chain = NULL;
        result("corewindow-foreign-iunknown", IDXGIFactory2_CreateSwapChainForCoreWindow(factory,
                (IUnknown *)device, (IUnknown *)factory, &desc, NULL, &chain));
        if (chain) IDXGISwapChain1_Release(chain);
        chain = NULL;
        result("composition", IDXGIFactory2_CreateSwapChainForComposition(factory, (IUnknown *)device,
                                                                            &desc, NULL, &chain));
        if (chain) {
            HWND hwnd;
            result("composition-hwnd", IDXGISwapChain1_GetHwnd(chain, &hwnd));
            result("composition-present", IDXGISwapChain1_Present(chain, 0, 0));
            IDXGISwapChain1_Release(chain);
            chain = NULL;
        }
        desc.SwapEffect = DXGI_SWAP_EFFECT_DISCARD;
        desc.BufferCount = 1;
        result("composition-discard", IDXGIFactory2_CreateSwapChainForComposition(factory, (IUnknown *)device,
                                                                                    &desc, NULL, &chain));
        if (chain) IDXGISwapChain1_Release(chain);
        chain = NULL;
        if (FAILED(IDXGIFactory2_EnumAdapters(factory, 0, &adapter))
            || FAILED(IDXGIAdapter_EnumOutputs(adapter, 0, &output))) goto done;
        IDXGIOutput_QueryInterface(output, &IID_IDXGIOutput1, (void **)&output1);
        IDXGIOutput_QueryInterface(output, &IID_IDXGIOutput5, (void **)&output5);
        gamma_rows("windowed", output, FALSE);
        result("take-null", IDXGIOutput_TakeOwnership(output, NULL, TRUE));
        result("take-shared", IDXGIOutput_TakeOwnership(output, (IUnknown *)device, FALSE));
        result("take-exclusive", IDXGIOutput_TakeOwnership(output, (IUnknown *)device, TRUE));
        gamma_rows("direct-owner", output, FALSE);
        result("take-shared-while-exclusive", IDXGIOutput_TakeOwnership(output, (IUnknown *)device, FALSE));
        IDXGIOutput_ReleaseOwnership(output);
        gamma_rows("post-release", output, FALSE);
        if (output1) {
            result("duplicate-output", IDXGIOutput1_DuplicateOutput(output1, (IUnknown *)device, &duplicate));
            if (duplicate) {
                result("duplicate-second", IDXGIOutput1_DuplicateOutput(output1, (IUnknown *)device, &second));
                if (second) IDXGIOutputDuplication_Release(second);
                duplication_rows(duplicate, device, context);
                IDXGIOutputDuplication_Release(duplicate);
                duplicate = NULL;
            }
        }
        if (output5) {
            DXGI_FORMAT formats[] = {DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM};
            result("duplicate1-zero-formats", IDXGIOutput5_DuplicateOutput1(output5,
                   (IUnknown *)device, 0, 0, NULL, &duplicate));
            if (duplicate) IDXGIOutputDuplication_Release(duplicate);
            duplicate = NULL;
            result("duplicate1", IDXGIOutput5_DuplicateOutput1(output5, (IUnknown *)device, 0,
                                                               ARRAYSIZE(formats), formats, &duplicate));
            if (duplicate) {
                duplication_rows(duplicate, device, context);
                IDXGIOutputDuplication_Release(duplicate);
            }
        }
        if (SUCCEEDED(IDXGIFactory2_CreateSwapChainForHwnd(factory, (IUnknown *)device, window,
                                                           &desc, NULL, NULL, &chain))) {
            SetForegroundWindow(window);
            display_rows(output, output1, chain, device, context);
        }
done:
        if (chain) IDXGISwapChain1_Release(chain);
        if (output5) IDXGIOutput5_Release(output5);
        if (output1) IDXGIOutput1_Release(output1);
        if (output) IDXGIOutput_Release(output);
        if (adapter) IDXGIAdapter_Release(adapter);
        if (factory) IDXGIFactory2_Release(factory);
        ID3D11DeviceContext_Release(context);
        ID3D11Device_Release(device);
    }
    DestroyWindow(window);
    return 0;
}
