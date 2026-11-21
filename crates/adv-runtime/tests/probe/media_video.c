/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 media_video.c -o media_video.exe -lstrmiids -lole32 -luuid
 * Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 -DGUEST_READS media_video.c -o media_video_read.exe -lstrmiids -lole32 -luuid
 */
#define COBJMACROS
#include <windows.h>
#include <dshow.h>
#include <stdio.h>
static void check(HRESULT hr, const char *where) { if (FAILED(hr)) { printf("FAIL %s %08lx\n", where, hr); ExitProcess(1); } }
#ifdef GUEST_READS
static void use_guest_reads(void)
{
    BYTE *entry = (BYTE *)GetProcAddress(GetModuleHandleW(L"kernelbase.dll"), "ReadFile");
    DWORD protect;
    /* Vary Wine's unused hot-patch padding while keeping ReadFile itself intact. */
    if (!entry || entry[-1] != 0xcc || !VirtualProtect(entry - 1, 1, PAGE_EXECUTE_READWRITE, &protect))
        ExitProcess(4);
    entry[-1] = 0x90;
    if (!VirtualProtect(entry - 1, 1, protect, &protect)) ExitProcess(5);
}
#endif
int main(void)
{
    IGraphBuilder *graph;
    IMediaControl *control;
    IMediaEvent *event;
    IVideoWindow *video;
    HWND owner, child;
    long code;
#ifdef GUEST_READS
    use_guest_reads();
#endif
    CoInitialize(NULL);
    owner = CreateWindowW(L"static", L"media", WS_POPUP | WS_VISIBLE, 40, 30, 160, 120, NULL, NULL, NULL, NULL);
    child = CreateWindowW(L"static", L"", WS_CHILD | WS_VISIBLE, 11, 13, 100, 80, owner, NULL, NULL, NULL);
    if (!owner || !child) return 2;
    UpdateWindow(owner);
    check(CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&graph), "graph");
    check(IGraphBuilder_RenderFile(graph, L"synthetic.mpg", NULL), "render");
    check(IGraphBuilder_QueryInterface(graph, &IID_IMediaControl, (void **)&control), "control");
    check(IGraphBuilder_QueryInterface(graph, &IID_IMediaEvent, (void **)&event), "event");
    check(IGraphBuilder_QueryInterface(graph, &IID_IVideoWindow, (void **)&video), "video");
    check(IVideoWindow_put_Owner(video, (OAHWND)child), "owner");
    check(IVideoWindow_put_WindowStyle(video, WS_CHILD), "style");
    check(IVideoWindow_SetWindowPosition(video, 17, 19, 32, 24), "position");
    check(IMediaControl_Run(control), "run");
    /* Keep the only application thread busy while host playback completes. */
    DWORD start = GetTickCount();
    while (GetTickCount() - start < 900) {
        volatile unsigned value = 0;
        unsigned i;
        for (i = 0; i < 1000; i++) value += i;
    }
    check(IMediaEvent_WaitForCompletion(event, 0, &code), "host completion");
    if (code != EC_COMPLETE) return 3;
    check(IMediaControl_Stop(control), "stop");
    IVideoWindow_Release(video); IMediaEvent_Release(event); IMediaControl_Release(control); IGraphBuilder_Release(graph);
    DestroyWindow(owner);
    CoUninitialize();
    puts("media video ok"); return 0;
}
