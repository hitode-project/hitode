/* Build: i686-w64-mingw32-gcc -O2 -nostdlib -Wl,--entry,_mainCRTStartup dshow_empty_graph.c -o dshow_empty_graph.exe -lstrmiids -luuid -lole32 -lkernel32 */
/* Owned empty-graph probe. SDK declarations establish every vtable call ABI. */
#define WIN32_LEAN_AND_MEAN
#define CINTERFACE
#define COBJMACROS
#include <windows.h>
#include <dshow.h>
#include <stddef.h>

_Static_assert(offsetof(IGraphBuilderVtbl, RenderFile) == 13 * 4, "IGraphBuilder RenderFile slot");
_Static_assert(offsetof(IMediaControlVtbl, GetState) == 10 * 4, "IMediaControl GetState slot");
_Static_assert(offsetof(IMediaEventExVtbl, SetNotifyWindow) == 13 * 4, "IMediaEventEx SetNotifyWindow slot");
_Static_assert(offsetof(IMediaSeekingVtbl, SetRate) == 17 * 4, "IMediaSeeking SetRate slot");
_Static_assert(offsetof(IBasicAudioVtbl, put_Volume) == 7 * 4, "IBasicAudio Volume slot");
_Static_assert(offsetof(IVideoWindowVtbl, SetWindowPosition) == 39 * 4, "IVideoWindow position slot");

static DWORD words[128], used;
static void record(DWORD value) { if (used == 128) ExitProcess(90); words[used++] = value; }
static void save(void) {
    DWORD written;
    HANDLE file = CreateFileA("dshow-empty-graph.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, words, used * 4, &written, NULL) || written != used * 4) ExitProcess(91);
    CloseHandle(file);
}
void mainCRTStartup(void) {
    IGraphBuilder *graph = NULL;
    record(CoInitializeEx(NULL, COINIT_APARTMENTTHREADED));
    record(CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&graph));
    if (!graph) ExitProcess(92);
    IUnknown *unknown = NULL;
    record(IGraphBuilder_QueryInterface(graph, &IID_IUnknown, (void **)&unknown));
    const IID *interfaces[] = { &IID_IGraphBuilder, &IID_IFilterGraph, &IID_IMediaControl,
        &IID_IMediaEventEx, &IID_IMediaEvent, &IID_IMediaSeeking, &IID_IBasicAudio, &IID_IVideoWindow };
    for (UINT i = 0; i != sizeof(interfaces) / sizeof(interfaces[0]); ++i) {
        IUnknown *alias = NULL, *canonical = NULL;
        record(IGraphBuilder_QueryInterface(graph, interfaces[i], (void **)&alias));
        if (!alias) ExitProcess(93);
        record(IUnknown_QueryInterface(alias, &IID_IUnknown, (void **)&canonical));
        record(canonical == unknown);
        if (canonical) IUnknown_Release(canonical);
        IUnknown_Release(alias);
    }
    IMediaControl *control = NULL;
    IMediaEventEx *events = NULL;
    IMediaSeeking *seeking = NULL;
    IGraphBuilder_QueryInterface(graph, &IID_IMediaControl, (void **)&control);
    IGraphBuilder_QueryInterface(graph, &IID_IMediaEventEx, (void **)&events);
    IGraphBuilder_QueryInterface(graph, &IID_IMediaSeeking, (void **)&seeking);
    record(IMediaControl_Stop(control));
    OAFilterState state = (OAFilterState)99;
    record(IMediaControl_GetState(control, 100, &state)); record(state);
    IEnumFilters *filters = NULL, *clone = NULL;
    record(IGraphBuilder_EnumFilters(graph, &filters));
    IBaseFilter *filter = NULL;
    ULONG fetched = 99;
    record(IEnumFilters_Next(filters, 1, &filter, &fetched)); record(fetched);
    record(IEnumFilters_Reset(filters)); record(IEnumFilters_Skip(filters, 1));
    record(IEnumFilters_Clone(filters, &clone));
    record(IEnumFilters_Next(clone, 1, &filter, &fetched)); record(fetched);
    IEnumFilters_Release(clone); IEnumFilters_Release(filters);
    record(IGraphBuilder_FindFilterByName(graph, L"authored-absent-filter", &filter)); record(filter == NULL);
    record(IMediaEventEx_SetNotifyFlags(events, 1));
    LONG flags = -1;
    record(IMediaEventEx_GetNotifyFlags(events, &flags)); record(flags);
    OAEVENT event = 0;
    record(IMediaEventEx_GetEventHandle(events, &event)); record(event != 0);
    record(WaitForSingleObject((HANDLE)event, 0));
    LONG code = 0; LONG_PTR p1 = 0, p2 = 0;
    record(IMediaEventEx_GetEvent(events, &code, &p1, &p2, 0));
    IID absent = { 0x42424242, 0x4242, 0x4242, { 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x42 } };
    IUnknown *missing = (IUnknown *)1;
    record(IGraphBuilder_QueryInterface(graph, &absent, (void **)&missing)); record(missing == NULL);
    IUnknown_Release(unknown); IMediaSeeking_Release(seeking);
    IMediaEventEx_Release(events); IMediaControl_Release(control); IGraphBuilder_Release(graph);
    CoUninitialize();
    record(0x44534801); save(); ExitProcess(0);
}
