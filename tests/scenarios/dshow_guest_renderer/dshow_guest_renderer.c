/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 dshow_guest_renderer.c -o dshow_guest_renderer.exe -lstrmiids -lole32 -ldmoguids -luuid */
/* A WMA stream decoded by the DMO wrapper reaches a guest DirectSound
 * renderer: the graph pauses with its preroll sample and runs to completion.
 * The guest AVI splitter pushes the compressed stream of tone.avi:
 * ffmpeg -f lavfi -i sine=frequency=440:sample_rate=44100:duration=1
 *   -ac 2 -c:a wmav2 -b:a 64k -map_metadata -1 -fflags +bitexact
 *   -flags:a +bitexact tone.avi
 * dshow-guest-renderer.bin holds each step's result. Exit status: 0 when the
 * graph paused, ran and completed; otherwise the failing step (20 and up). */
#define COBJMACROS
#include <windows.h>
#include <dshow.h>
#include <dmodshow.h>

static const GUID WMA_DECODER = {0x2eeb4adf, 0x4578, 0x4d10, {0xbc, 0xa7, 0xbb, 0x95, 0x5f, 0x56, 0x32, 0x0a}};
static const GUID AUDIO_DECODERS = {0x57f2db8b, 0xe6bb, 0x4513, {0x9d, 0x43, 0xdc, 0xd2, 0xa6, 0x59, 0x31, 0x25}};

static HANDLE out;
static void put(DWORD value) {
    DWORD n;
    if (!WriteFile(out, &value, 4, &n, NULL) || n != 4) ExitProcess(2);
}

static void step(int number, HRESULT hr) {
    put((DWORD)hr);
    if (FAILED(hr)) {
        CloseHandle(out);
        ExitProcess(20 + number);
    }
}

static IPin *pin(IBaseFilter *filter, PIN_DIRECTION wanted) {
    IEnumPins *pins;
    IPin *found = NULL, *each;
    PIN_DIRECTION direction;
    if (FAILED(IBaseFilter_EnumPins(filter, &pins))) return NULL;
    while (!found && IEnumPins_Next(pins, 1, &each, NULL) == S_OK) {
        if (SUCCEEDED(IPin_QueryDirection(each, &direction)) && direction == wanted)
            found = each;
        else
            IPin_Release(each);
    }
    IEnumPins_Release(pins);
    return found;
}

int main(void) {
    IGraphBuilder *graph;
    IBaseFilter *source, *splitter, *decoder, *renderer;
    IDMOWrapperFilter *wrapper;
    IFileSourceFilter *file;
    IMediaControl *control;
    IMediaEvent *events;
    OAFilterState state = -1;
    LONG code = 0;
    HRESULT hr;

    out = CreateFileA("dshow-guest-renderer.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS,
                      FILE_ATTRIBUTE_NORMAL, NULL);
    if (out == INVALID_HANDLE_VALUE) return 3;
    step(0, CoInitialize(NULL));
    step(1, CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder,
                             (void **)&graph));
    step(2, CoCreateInstance(&CLSID_DSoundRender, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter,
                             (void **)&renderer));
    step(3, CoCreateInstance(&CLSID_DMOWrapperFilter, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter,
                             (void **)&decoder));
    step(4, IBaseFilter_QueryInterface(decoder, &IID_IDMOWrapperFilter, (void **)&wrapper));
    step(5, IDMOWrapperFilter_Init(wrapper, &WMA_DECODER, &AUDIO_DECODERS));
    step(6, IGraphBuilder_AddFilter(graph, renderer, L"renderer"));
    step(7, IGraphBuilder_AddFilter(graph, decoder, L"decoder"));
    step(8, CoCreateInstance(&CLSID_AsyncReader, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&source));
    step(8, IGraphBuilder_AddFilter(graph, source, L"source"));
    step(8, IBaseFilter_QueryInterface(source, &IID_IFileSourceFilter, (void **)&file));
    step(8, IFileSourceFilter_Load(file, L"tone.avi", NULL));
    step(9, CoCreateInstance(&CLSID_AviSplitter, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&splitter));
    step(9, IGraphBuilder_AddFilter(graph, splitter, L"splitter"));
    step(9, IGraphBuilder_ConnectDirect(graph, pin(source, PINDIR_OUTPUT), pin(splitter, PINDIR_INPUT), NULL));
    step(10, IGraphBuilder_ConnectDirect(graph, pin(splitter, PINDIR_OUTPUT), pin(decoder, PINDIR_INPUT), NULL));
    step(10, IGraphBuilder_ConnectDirect(graph, pin(decoder, PINDIR_OUTPUT), pin(renderer, PINDIR_INPUT), NULL));
    step(11, IGraphBuilder_QueryInterface(graph, &IID_IMediaControl, (void **)&control));
    step(12, IGraphBuilder_QueryInterface(graph, &IID_IMediaEvent, (void **)&events));
    hr = IMediaControl_Pause(control);
    put((DWORD)hr);
    /* A renderer still waiting for its preroll sample reports an
     * intermediate state when the wait ends. */
    hr = IMediaControl_GetState(control, 5000, &state);
    put((DWORD)hr);
    put((DWORD)state);
    if (hr != S_OK || state != State_Paused) {
        CloseHandle(out);
        return 40;
    }
    step(13, IMediaControl_Run(control));
    hr = IMediaEvent_WaitForCompletion(events, 5000, &code);
    put((DWORD)hr);
    put((DWORD)code);
    IMediaControl_Stop(control);
    CloseHandle(out);
    return hr == S_OK && code == EC_COMPLETE ? 0 : 41;
}
