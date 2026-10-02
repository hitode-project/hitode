#include "win32_min.h"

API int __cdecl ProbeAdd(int);
API int __cdecl ProbeReadTls(void);
API void __cdecl ProbeSetTls(int);
API DWORD __cdecl ProbeBase(void);

static HANDLE ready_event;
static volatile DWORD worker_result;
static volatile UINT input_mask;
static volatile UINT character_count;
static volatile WPARAM character_value;
static UINT ticks;
static HDC memory_dc;
static DWORD *pixels;
static HBITMAP bitmap;
static HWND window;

static DWORD WINAPI worker(void *unused) {
    (void)unused;
    /* The main thread changed its TLS copy; a new thread gets the template. */
    worker_result = ProbeReadTls() == 37 ? 1 : 0;
    Sleep(15);
    SetEvent(ready_event);
    return 0;
}

static void draw_to(HDC dc) {
    int x, y;
    if (!pixels) return;
    for (y = 0; y < 240; ++y) {
        for (x = 0; x < 320; ++x) {
            DWORD red = (DWORD)((x * 255) / 319);
            DWORD green = (DWORD)((y * 255) / 239);
            DWORD blue = (DWORD)((x ^ y) & 255);
            pixels[y * 320 + x] = 0xff000000u | (red << 16) | (green << 8) | blue;
        }
    }
    /* Input state is visible as two bright squares. A third proves timer callback
       resumption after Sleep; deterministic screenshots are compared in tests. */
    for (y = 12; y < 36; ++y) for (x = 12; x < 36; ++x)
        pixels[y * 320 + x] = input_mask & 1 ? 0xffffffffu : 0xff800000u;
    for (y = 12; y < 36; ++y) for (x = 44; x < 68; ++x)
        pixels[y * 320 + x] = input_mask & 2 ? 0xffffffffu : 0xff008000u;
    for (y = 12; y < 36; ++y) for (x = 76; x < 100; ++x)
        pixels[y * 320 + x] = ticks >= 3 ? 0xffffffffu : 0xff000080u;
    if (!BitBlt(dc, 0, 0, 320, 240, memory_dc, 0, 0, SRCCOPY)) ExitProcess(21);
    GdiFlush();
}

static void draw(void) {
    HDC dc;
    if (!pixels) return;
    dc = GetDC(window);
    draw_to(dc);
    ReleaseDC(window, dc);
}

static void WINAPI timer_callback(HWND hwnd, UINT message, UINT id, DWORD time) {
    (void)message; (void)time;
    Sleep(3);
    ++ticks;
    draw();
    if (ticks >= 3) {
        KillTimer(hwnd, id);
        /* Give a human a short window to inspect. Acceptance tests post input. */
        if (input_mask == 3) {
            if (character_count != 1 || character_value != 'a') ExitProcess(23);
            DestroyWindow(hwnd);
        }
        else SetTimer(hwnd, 2, 1000, NULL);
    }
}

static LRESULT WINAPI window_proc(HWND hwnd, UINT message, WPARAM wp, LPARAM lp) {
    if (message == WM_KEYDOWN) { input_mask |= 1; draw(); return 0; }
    if (message == 0x102) { ++character_count; character_value = wp; return 0; }
    if (message == WM_LBUTTONDOWN) { input_mask |= 2; draw(); return 0; }
    if (message == WM_PAINT) {
        PAINTSTRUCT paint;
        HDC dc = BeginPaint(hwnd, &paint);
        if (pixels) draw_to(dc);
        EndPaint(hwnd, &paint); return 0;
    }
    if (message == 0x113 && wp == 2) { KillTimer(hwnd, 2); DestroyWindow(hwnd); return 0; }
    if (message == WM_CLOSE) { DestroyWindow(hwnd); return 0; }
    if (message == WM_DESTROY) { PostQuitMessage(input_mask == 3 ? 0 : 22); return 0; }
    return DefWindowProcA(hwnd, message, wp, lp);
}

void mainCRTStartup(void) {
    HMODULE dynamic;
    int (__cdecl *dynamic_add)(int);
    DWORD (__cdecl *dynamic_base)(void);
    HANDLE thread;
    WNDCLASSA klass = {0};
    BITMAPINFO info = {0};
    MSG message;
    if (ProbeAdd(23) != 33) ExitProcess(10);
    if (ProbeBase() == 0x00400000u || ProbeBase() == 0) ExitProcess(11);
    if (ProbeReadTls() != 37) ExitProcess(12);
    ProbeSetTls(91);
    if (ProbeReadTls() != 91) ExitProcess(13);
    dynamic = LoadLibraryA("fixture_dynamic.dll");
    if (!dynamic) ExitProcess(14);
    dynamic_add = (int (__cdecl *)(int))GetProcAddress(dynamic, "ProbeAdd");
    dynamic_base = (DWORD (__cdecl *)(void))GetProcAddress(dynamic, (const char *)4);
    if (!dynamic_add || dynamic_add(23) != 33 || !dynamic_base || dynamic_base() == 0x00400000u) ExitProcess(15);
    ready_event = CreateEventA(NULL, 0, 0, NULL);
    thread = CreateThread(NULL, 0, worker, NULL, 0, NULL);
    if (!ready_event || !thread || WaitForSingleObject(ready_event, INFINITE) != 0 || worker_result != 1) ExitProcess(16);
    if (WaitForSingleObject(thread, INFINITE) != 0) ExitProcess(17);
    CloseHandle(thread); CloseHandle(ready_event); FreeLibrary(dynamic);
    klass.procedure = window_proc;
    klass.instance = GetModuleHandleA(NULL);
    klass.name = "AdvFixtureWindow";
    if (!RegisterClassA(&klass)) ExitProcess(18);
    window = CreateWindowExA(0, klass.name, "ADV PE fixture: keyboard / mouse / timer", 0x10cf0000u,
        0, 0, 336, 279, 0, 0, klass.instance, NULL);
    if (!window) ExitProcess(19);
    memory_dc = CreateCompatibleDC(0);
    info.header.size = sizeof(BITMAPINFOHEADER);
    info.header.width = 320; info.header.height = -240;
    info.header.planes = 1; info.header.bpp = 32;
    bitmap = CreateDIBSection(memory_dc, &info, 0, (void **)&pixels, 0, 0);
    if (!memory_dc || !bitmap || !pixels) ExitProcess(20);
    SelectObject(memory_dc, bitmap);
    ShowWindow(window, 5); UpdateWindow(window);
    draw();
    SetTimer(window, 1, 15, timer_callback);
    while (GetMessageA(&message, 0, 0, 0) > 0) {
        TranslateMessage(&message); DispatchMessageA(&message);
    }
    DeleteObject(bitmap); DeleteDC(memory_dc);
    ExitProcess(message.wparam);
}
