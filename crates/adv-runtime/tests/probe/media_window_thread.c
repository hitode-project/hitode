/* Which thread owns the default video renderer's window, and whether
 * IVideoWindow calls from another thread complete while the thread that
 * rendered the file waits without pumping messages.
 * Build: i686-w64-mingw32-gcc -O1 -municode -o media_window_thread.exe media_window_thread.c -lole32 -loleaut32 -lstrmiids -luuid */
#define COBJMACROS
#include <windows.h>
#include <dshow.h>
#include <stdio.h>

static const WCHAR *path;
static HANDLE rendered, finished;
static IGraphBuilder *graph;
static IVideoWindow *video;
static DWORD render_thread;

static BOOL CALLBACK find(HWND hwnd, LPARAM found)
{
    WCHAR text[64];
    if (InternalGetWindowText(hwnd, text, 64) && !lstrcmpW(text, L"ActiveMovie Window"))
    {
        *(HWND *)found = hwnd;
        return FALSE;
    }
    return TRUE;
}

static DWORD WINAPI render(void *unused)
{
    CoInitializeEx(NULL, COINIT_MULTITHREADED);
    render_thread = GetCurrentThreadId();
    HRESULT hr = CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder,
                                  (void **)&graph);
    if (SUCCEEDED(hr)) hr = IGraphBuilder_RenderFile(graph, path, NULL);
    if (SUCCEEDED(hr)) hr = IGraphBuilder_QueryInterface(graph, &IID_IVideoWindow, (void **)&video);
    printf("render=%08lx\n", hr);
    fflush(stdout);
    SetEvent(rendered);
    /* No message pumping from here on. */
    WaitForSingleObject(finished, INFINITE);
    return 0;
}

static DWORD WINAPI position(void *unused)
{
    HRESULT hr = IVideoWindow_SetWindowPosition(video, 0, 0, 320, 240);
    printf("position=%08lx\n", hr);
    hr = IVideoWindow_put_Visible(video, OATRUE);
    printf("visible=%08lx\n", hr);
    fflush(stdout);
    return 0;
}

int wmain(int argc, WCHAR **argv)
{
    path = argc > 1 ? argv[1] : L"synthetic.wmv";
    rendered = CreateEventW(NULL, TRUE, FALSE, NULL);
    finished = CreateEventW(NULL, TRUE, FALSE, NULL);
    HANDLE renderer = CreateThread(NULL, 0, render, NULL, 0, NULL);
    WaitForSingleObject(rendered, INFINITE);
    if (!video) return 1;
    HWND hwnd = NULL;
    EnumWindows(find, (LPARAM)&hwnd);
    DWORD owner = hwnd ? GetWindowThreadProcessId(hwnd, NULL) : 0;
    printf("window=%d owner=%s\n", hwnd != NULL,
           !owner ? "none" : owner == render_thread ? "render" :
           owner == GetCurrentThreadId() ? "main" : "own");
    fflush(stdout);
    HANDLE mover = CreateThread(NULL, 0, position, NULL, 0, NULL);
    DWORD wait = WaitForSingleObject(mover, 5000);
    printf("call=%s\n", wait == WAIT_OBJECT_0 ? "returned" : "blocked");
    fflush(stdout);
    if (wait != WAIT_OBJECT_0) ExitProcess(3);
    SetEvent(finished);
    WaitForSingleObject(renderer, INFINITE);
    IVideoWindow_put_Visible(video, OAFALSE);
    IVideoWindow_Release(video);
    IGraphBuilder_Release(graph);
    if (owner && owner != render_thread && owner != GetCurrentThreadId()) printf("media window_thread ok\n");
    return 0;
}
