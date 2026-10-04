#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>

int _fltused;
#ifdef GUEST_VISIBLE
#define FIXTURE_STYLE (WS_OVERLAPPED | WS_VISIBLE)
#else
#define FIXTURE_STYLE WS_OVERLAPPED
#endif
void *memset(void *p, int byte, unsigned int n) {
    unsigned char *out = p;
    while (n--) *out++ = (unsigned char)byte;
    return p;
}
static DWORD results[128];
static UINT used;
static void record(DWORD value) { results[used++] = value; }
static void observe(void) {
    unsigned short cw;
    DWORD mxcsr;
    __asm { fnstcw cw }
    __asm { stmxcsr mxcsr }
    record(cw); record(mxcsr);
}
static void configure(unsigned short cw, DWORD mxcsr) {
    __asm { fnclex }
    __asm { fldcw cw }
    __asm { ldmxcsr mxcsr }
}
static void save(void) {
    DWORD wrote;
    HANDLE file = CreateFileA("d3d9-environment.bin", GENERIC_WRITE, 0, NULL,
        CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(90);
    if (!WriteFile(file, results, used * sizeof(DWORD), &wrote, NULL) ||
        wrote != used * sizeof(DWORD)) ExitProcess(91);
    CloseHandle(file);
}
static void success(HRESULT hr) {
    record(hr);
    if (FAILED(hr)) { save(); ExitProcess(94); }
}
void mainCRTStartup(void) {
    HWND window = CreateWindowExA(WS_EX_NOACTIVATE, "STATIC", "D3D9 FPU silent fixture", FIXTURE_STYLE,
        -32000, -32000, 64, 64, NULL, NULL, GetModuleHandleA(NULL), NULL);
    IDirect3D9 *factory = Direct3DCreate9(D3D_SDK_VERSION);
    UINT index;
    if (!window || !factory) ExitProcess(1);
    for (index = 0; index < 5; ++index) {
        IDirect3DDevice9 *device = NULL;
        D3DPRESENT_PARAMETERS parameters = {0};
        DWORD preserve = index & 1 ? D3DCREATE_FPU_PRESERVE : 0;
        unsigned short cw = index < 2 ? 0x0f7f : index < 4 ? 0x077f : 0x1f7e;
        DWORD mxcsr = index < 2 ? 0xffc0 : index < 4 ? 0x3fc0 : 0x1f83;
        parameters.BackBufferWidth = 16;
        parameters.BackBufferHeight = 12;
        parameters.BackBufferFormat = D3DFMT_A8R8G8B8;
        parameters.BackBufferCount = 1;
        parameters.SwapEffect = D3DSWAPEFFECT_DISCARD;
        parameters.hDeviceWindow = window;
        parameters.Windowed = TRUE;
        parameters.PresentationInterval = index < 2 ? D3DPRESENT_INTERVAL_IMMEDIATE
            : index == 3 ? D3DPRESENT_INTERVAL_ONE : D3DPRESENT_INTERVAL_DEFAULT;
        configure(cw, mxcsr);
        observe();
        success(IDirect3D9_CreateDevice(factory, 0, D3DDEVTYPE_HAL, window,
            D3DCREATE_SOFTWARE_VERTEXPROCESSING | preserve, &parameters, &device));
        observe();
        configure(cw, mxcsr);
        observe();
        success(IDirect3DDevice9_Reset(device, &parameters));
        observe();
        success(IDirect3DDevice9_Present(device, NULL, NULL, NULL, NULL));
        observe();
        record(IDirect3DDevice9_Release(device));
        observe();
    }
    configure(0x027f, 0x1f80);
    record(IDirect3D9_Release(factory));
    {
        volatile DWORD guard = 0;
        for (index = 0; index < 50000; ++index) guard = guard * 1664525u + index;
        record(guard);
    }
    save();
    DestroyWindow(window);
    ExitProcess(0);
}
