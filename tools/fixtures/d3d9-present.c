/* Authored base-D3D9 visibility oracle. Offscreen, no activation, no sound. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>

int _fltused;
void *memset(void *p, int byte, unsigned int n) {
    unsigned char *out = p;
    while (n--) *out++ = (unsigned char)byte;
    return p;
}
static DWORD words[91], used;
static void record(DWORD value) {
    if (used == 91) ExitProcess(89);
    words[used++] = value;
}
static void save(void) {
    DWORD wrote;
    HANDLE file = CreateFileA("d3d9-present.bin", GENERIC_WRITE, 0, NULL,
        CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(90);
    if (!WriteFile(file, words, used * sizeof(DWORD), &wrote, NULL) ||
        wrote != used * sizeof(DWORD)) ExitProcess(91);
    CloseHandle(file);
}
void mainCRTStartup(void) {
    static const DWORD intervals[3] = {
        D3DPRESENT_INTERVAL_DEFAULT, D3DPRESENT_INTERVAL_ONE,
        D3DPRESENT_INTERVAL_IMMEDIATE
    };
    static const int commands[5] = {
        SW_HIDE, SW_SHOWNOACTIVATE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SW_HIDE
    };
    IDirect3D9 *factory = Direct3DCreate9(D3D_SDK_VERSION);
    UINT index, state;
    if (!factory) ExitProcess(1);
    for (index = 0; index != 3; ++index) {
        HWND window = CreateWindowExA(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, "STATIC",
            "D3D9 base Present silent fixture", WS_OVERLAPPED,
            -32000, -32000, 64, 64, NULL, NULL, GetModuleHandleA(NULL), NULL);
        D3DPRESENT_PARAMETERS parameters = {0};
        IDirect3DDevice9 *device = NULL;
        HRESULT hr;
        if (!window) ExitProcess(1);
        parameters.BackBufferWidth = 16;
        parameters.BackBufferHeight = 12;
        parameters.BackBufferFormat = D3DFMT_A8R8G8B8;
        parameters.BackBufferCount = 1;
        parameters.SwapEffect = D3DSWAPEFFECT_DISCARD;
        parameters.hDeviceWindow = window;
        parameters.Windowed = TRUE;
        parameters.PresentationInterval = intervals[index];
        record(intervals[index]);
        hr = IDirect3D9_CreateDevice(factory, 0, D3DDEVTYPE_HAL, window,
            D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE,
            &parameters, &device);
        record(hr);
        if (FAILED(hr)) { save(); ExitProcess(2); }
        for (state = 0; state != 5; ++state) {
            /* First SW_HIDE also consumes the launcher's STARTUPINFO show state. */
            ShowWindow(window, commands[state]);
            record(state);
            record(IsWindowVisible(window) != 0);
            record(IsIconic(window) != 0);
            hr = IDirect3DDevice9_Present(device, NULL, NULL, NULL, NULL);
            record(hr);
            if (FAILED(hr)) { save(); ExitProcess(3); }
            record(IDirect3DDevice9_TestCooperativeLevel(device));
            if (state == 2) {
                ShowWindow(window, SW_SHOWNA);
                record(IsWindowVisible(window) != 0);
                record(IsIconic(window) != 0);
            }
        }
        record(IDirect3DDevice9_Release(device));
        DestroyWindow(window);
    }
    record(IDirect3D9_Release(factory));
    /* Give the shared compiler time to publish this authored hot loop. */
    {
        volatile DWORD guard = 0;
        for (index = 0; index != 50000; ++index) guard = guard * 1664525u + index;
    }
    save();
    ExitProcess(0);
}
