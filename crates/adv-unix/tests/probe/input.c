/* Host input probe: a decorated window at a nonzero screen origin waits
 * for mouse and keyboard hardware input, then exits on F12's release.
 * Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O1 -o input.exe input.c -lgdi32 -luser32
 * DirectDraw: add -DINPUT_DDRAW -lddraw -ldxguid, output input_ddraw.exe.
 * Direct3D 9: add -DINPUT_D3D9 -ld3d9, output input_d3d9.exe. */
#define COBJMACROS
#include <windows.h>
#ifdef INPUT_DDRAW
#include <ddraw.h>
#endif
#ifdef INPUT_D3D9
#include <d3d9.h>
#endif
#include <stdio.h>

static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message >= WM_MOUSEMOVE && message <= WM_MOUSEWHEEL)
    {
        POINT point = {(short)LOWORD(lparam), (short)HIWORD(lparam)};
        if (message == WM_MOUSEWHEEL) ScreenToClient(window, &point);
        printf("mouse=%x point=%ld,%ld data=%d\n", message, point.x, point.y,
               message == WM_MOUSEWHEEL ? (short)HIWORD(wparam) : 0);
        fflush(stdout);
        return 0;
    }
    if (message == WM_KEYDOWN || message == WM_KEYUP ||
        message == WM_SYSKEYDOWN || message == WM_SYSKEYUP)
    {
        printf("key=%x vk=%u scan=%u extended=%u\n", message, (unsigned)wparam,
               (unsigned)(lparam >> 16) & 255, (unsigned)(lparam >> 24) & 1);
        fflush(stdout);
        if (message == WM_KEYUP && wparam == VK_F12) PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

#ifdef INPUT_DDRAW
static int present(HWND window)
{
    IDirectDraw7 *draw;
    IDirectDrawSurface7 *primary, *source;
    IDirectDrawClipper *clipper;
    DDSURFACEDESC2 desc = {sizeof(desc)};
    DDBLTFX fx = {sizeof(fx)};
    RECT rect;
    HRESULT result;
    if (FAILED(DirectDrawCreateEx(NULL, (void **)&draw, &IID_IDirectDraw7, NULL))) return 0;
    if (FAILED(IDirectDraw7_SetCooperativeLevel(draw, window, DDSCL_NORMAL))) return 0;
    desc.dwFlags = DDSD_CAPS;
    desc.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
    if (FAILED(IDirectDraw7_CreateSurface(draw, &desc, &primary, NULL))) return 0;
    if (FAILED(IDirectDraw7_CreateClipper(draw, 0, &clipper, NULL))) return 0;
    IDirectDrawClipper_SetHWnd(clipper, 0, window);
    IDirectDrawSurface7_SetClipper(primary, clipper);
    GetClientRect(window, &rect);
    desc.dwFlags = DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT;
    desc.ddsCaps.dwCaps = DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY;
    desc.dwWidth = rect.right;
    desc.dwHeight = rect.bottom;
    if (FAILED(IDirectDraw7_CreateSurface(draw, &desc, &source, NULL))) return 0;
    fx.dwFillColor = 0xff0000;
    IDirectDrawSurface7_Blt(source, NULL, NULL, NULL, DDBLT_COLORFILL | DDBLT_WAIT, &fx);
    MapWindowPoints(window, NULL, (POINT *)&rect, 2);
    result = IDirectDrawSurface7_Blt(primary, &rect, source, NULL, DDBLT_WAIT, NULL);
    IDirectDrawSurface7_Release(source);
    IDirectDrawClipper_Release(clipper);
    IDirectDrawSurface7_Release(primary);
    IDirectDraw7_Release(draw);
    return SUCCEEDED(result);
}
#endif

#ifdef INPUT_D3D9
static int present(HWND window)
{
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    IDirect3DDevice9 *device;
    D3DPRESENT_PARAMETERS params = {0};
    HRESULT result;
    if (!d3d) return 0;
    params.Windowed = TRUE;
    params.SwapEffect = D3DSWAPEFFECT_DISCARD;
    params.BackBufferFormat = D3DFMT_X8R8G8B8;
    params.hDeviceWindow = window;
    if (FAILED(IDirect3D9_CreateDevice(d3d, 0, D3DDEVTYPE_HAL, window,
                                      D3DCREATE_SOFTWARE_VERTEXPROCESSING, &params, &device))) return 0;
    IDirect3DDevice9_Clear(device, 0, NULL, D3DCLEAR_TARGET, 0xffff0000, 1.0f, 0);
    result = IDirect3DDevice9_Present(device, NULL, NULL, NULL, NULL);
    IDirect3DDevice9_Release(device);
    IDirect3D9_Release(d3d);
    return SUCCEEDED(result);
}
#endif

int main(void)
{
    WNDCLASSW class = {0};
    HWND window;
    RECT rect;
    POINT client = {0, 0};
    MSG message;

    class.lpfnWndProc = procedure;
    class.hInstance = GetModuleHandleW(NULL);
    class.lpszClassName = L"input-probe";
    class.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    if (!RegisterClassW(&class)) return 2;
    window = CreateWindowExW(0, class.lpszClassName, L"probe", WS_OVERLAPPEDWINDOW,
                             70, 90, 240, 180, NULL, NULL, class.hInstance, NULL);
    if (!window) return 3;
    ShowWindow(window, SW_SHOW);
    SetForegroundWindow(window);
    SetFocus(window);
    UpdateWindow(window);
    GetWindowRect(window, &rect);
    ClientToScreen(window, &client);
    while (PeekMessageW(&message, NULL, 0, 0, PM_REMOVE)) DispatchMessageW(&message);
#if defined(INPUT_DDRAW) || defined(INPUT_D3D9)
    if (!present(window)) return 4;
    printf("ready=0,0\n");
#else
    printf("ready=%ld,%ld\n", client.x - rect.left, client.y - rect.top);
#endif
    fflush(stdout);
    while (GetMessageW(&message, NULL, 0, 0) > 0)
        DispatchMessageW(&message);
    DestroyWindow(window);
    return 0;
}
