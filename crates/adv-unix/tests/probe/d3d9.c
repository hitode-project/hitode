/* The Direct3D 9 probe: GDI draws a TrueType string into an offscreen plain
 * surface through the surface's DC, and the surface is read back through
 * LockRect. GDI then draws the same into the back buffer's DC, which is
 * presented. It exits with 0 only when the text inked the surface, the rest
 * kept the fill and the back buffer drew and presented.
 * Build: i686-w64-mingw32-gcc -O1 -o d3d9.exe d3d9.c -ld3d9 -lgdi32 -luser32 */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
#include <stdio.h>
#include <string.h>

#define WIDTH 64
#define HEIGHT 32
#define WHITE 0xffffff
#define BLUE 0x0000ff

static int draw(IDirect3DSurface9 *surface, HRESULT *released)
{
    RECT rect = {0, 0, WIDTH, HEIGHT};
    HBRUSH blue = CreateSolidBrush(RGB(0, 0, 255));
    HFONT font = CreateFontW(-16, 0, 0, 0, FW_NORMAL, 0, 0, 0, ANSI_CHARSET, 0, 0,
                             NONANTIALIASED_QUALITY, 0, L"Tahoma");
    HRESULT got;
    int drawn = 0;
    HDC dc;

    got = IDirect3DSurface9_GetDC(surface, &dc);
    if (SUCCEEDED(got))
    {
        FillRect(dc, &rect, blue);
        SelectObject(dc, font);
        SetTextColor(dc, RGB(255, 255, 255));
        SetBkMode(dc, TRANSPARENT);
        drawn = TextOutW(dc, 4, 4, L"Hi", 2);
        *released = IDirect3DSurface9_ReleaseDC(surface, dc);
    }
    DeleteObject(font);
    DeleteObject(blue);
    return SUCCEEDED(got) ? drawn : got;
}

int main(void)
{
    if (GetFileAttributesA("spawn-child") != INVALID_FILE_ATTRIBUTES && !strstr(GetCommandLineA(), " child")) {
        char path[MAX_PATH], command[MAX_PATH + 16];
        STARTUPINFOA startup = {sizeof(startup)};
        PROCESS_INFORMATION child;
        DWORD code;
        GetModuleFileNameA(NULL, path, sizeof(path));
        snprintf(command, sizeof(command), "\"%s\" child", path);
        if (!CreateProcessA(path, command, NULL, NULL, TRUE, 0, NULL, NULL, &startup, &child)) return 5;
        WaitForSingleObject(child.hProcess, INFINITE);
        if (!GetExitCodeProcess(child.hProcess, &code)) return 6;
        CloseHandle(child.hThread);
        CloseHandle(child.hProcess);
        return code;
    }
    D3DPRESENT_PARAMETERS params = {0};
    IDirect3DSurface9 *surface, *back;
    IDirect3DDevice9 *device;
    D3DLOCKED_RECT locked = {0};
    IDirect3D9 *d3d;
    HRESULT created, made, released = E_FAIL, lock, backreleased = E_FAIL, presented = E_FAIL;
    int drawn, backdrawn = 0, inked = 0, background = 0, other = 0;
    HWND window;

    if (!(d3d = Direct3DCreate9(D3D_SDK_VERSION))) return 2;
    window = CreateWindowExW(0, L"static", L"probe", WS_POPUP | WS_VISIBLE, 0, 0, WIDTH, HEIGHT,
                             NULL, NULL, NULL, NULL);
    params.Windowed = TRUE;
    params.SwapEffect = D3DSWAPEFFECT_COPY;
    params.BackBufferWidth = WIDTH;
    params.BackBufferHeight = HEIGHT;
    params.BackBufferFormat = D3DFMT_X8R8G8B8;
    params.hDeviceWindow = window;
    params.Flags = D3DPRESENTFLAG_LOCKABLE_BACKBUFFER;
    created = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, window,
                                      D3DCREATE_SOFTWARE_VERTEXPROCESSING, &params, &device);
    if (FAILED(created))
    {
        printf("created=%#lx\n", created);
        return 3;
    }
    made = IDirect3DDevice9_CreateOffscreenPlainSurface(device, WIDTH, HEIGHT, D3DFMT_X8R8G8B8,
                                                        D3DPOOL_SYSTEMMEM, &surface, NULL);
    if (FAILED(made)) return 4;

    drawn = draw(surface, &released);

    lock = IDirect3DSurface9_LockRect(surface, &locked, NULL, D3DLOCK_READONLY);
    if (SUCCEEDED(lock))
    {
        int x, y;
        for (y = 0; y < HEIGHT; y++)
            for (x = 0; x < WIDTH; x++)
            {
                DWORD pixel = ((DWORD *)((BYTE *)locked.pBits + y * locked.Pitch))[x] & 0xffffff;
                if (pixel == WHITE) inked++;
                else if (pixel == BLUE) background++;
                else other++;
            }
        IDirect3DSurface9_UnlockRect(surface);
    }

    if (SUCCEEDED(IDirect3DDevice9_GetBackBuffer(device, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back)))
    {
        backdrawn = draw(back, &backreleased);
        IDirect3DSurface9_Release(back);
        presented = IDirect3DDevice9_Present(device, NULL, NULL, NULL, NULL);
    }

    IDirect3DSurface9_Release(surface);
    IDirect3DDevice9_Release(device);
    IDirect3D9_Release(d3d);
    DestroyWindow(window);

    printf("drawn=%#x released=%#lx pitch=%d inked=%d background=%d other=%d "
           "backdrawn=%#x backreleased=%#lx presented=%#lx\n",
           drawn, released, locked.Pitch, inked, background, other, backdrawn, backreleased,
           presented);
    fflush(stdout);
    return !(drawn == 1 && released == D3D_OK && inked > 0 &&
             background + inked == WIDTH * HEIGHT && backdrawn == 1 && backreleased == D3D_OK &&
             presented == D3D_OK);
}
