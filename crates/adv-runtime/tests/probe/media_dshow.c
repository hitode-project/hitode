/* i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 media_dshow.c -o media_dshow.exe -lstrmiids -lole32 -loleaut32 -luuid */
#define COBJMACROS
#include <windows.h>
#include <dshow.h>
#include <qedit.h>
#include <stdio.h>
static unsigned frames;
static REFERENCE_TIME last;
static HRESULT WINAPI query(ISampleGrabberCB *self, REFIID iid, void **out) {
    if (IsEqualIID(iid, &IID_IUnknown) || IsEqualIID(iid, &IID_ISampleGrabberCB)) { *out = self; return S_OK; }
    *out = NULL; return E_NOINTERFACE;
}
static ULONG WINAPI retain(ISampleGrabberCB *self) { return 2; }
static ULONG WINAPI release(ISampleGrabberCB *self) { return 1; }
static HRESULT WINAPI sample_cb(ISampleGrabberCB *self, double time, IMediaSample *sample)
{
    BYTE *data;
    REFERENCE_TIME start, end;
    IMediaSample_GetTime(sample, &start, &end);
    if (frames && start < last) return E_FAIL;
    last = start;
    IMediaSample_GetPointer(sample, &data);
    if (data[0] > 12 || data[1] > 12 || data[2] < 230) return E_FAIL;
    frames++;
    return S_OK;
}
static HRESULT WINAPI buffer_cb(ISampleGrabberCB *self, double time, BYTE *buffer, long len) { return E_NOTIMPL; }
static ISampleGrabberCBVtbl callbacks = {query, retain, release, sample_cb, buffer_cb};
static ISampleGrabberCB callback = {&callbacks};
static void check(HRESULT hr, const char *where) { if (FAILED(hr)) { printf("FAIL %s %08lx\n", where, hr); ExitProcess(1); } }
static IPin *pin(IBaseFilter *filter, PIN_DIRECTION direction)
{
    IEnumPins *pins;
    IPin *pin;
    IBaseFilter_EnumPins(filter, &pins);
    while (IEnumPins_Next(pins, 1, &pin, NULL) == S_OK) {
        PIN_DIRECTION dir;
        IPin_QueryDirection(pin, &dir);
        if (dir == direction) { IEnumPins_Release(pins); return pin; }
        IPin_Release(pin);
    }
    IEnumPins_Release(pins); return NULL;
}
static void run(const WCHAR *file)
{
    IGraphBuilder *graph;
    IMediaControl *control;
    IMediaEvent *event;
    IBaseFilter *grabber, *sink;
    ISampleGrabber *samples;
    IPin *out, *in;
    AM_MEDIA_TYPE type = {0};
    long code;
    check(CoCreateInstance(&CLSID_FilterGraph, NULL, CLSCTX_INPROC_SERVER, &IID_IGraphBuilder, (void **)&graph), "graph");
    check(CoCreateInstance(&CLSID_SampleGrabber, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&grabber), "grabber");
    check(CoCreateInstance(&CLSID_NullRenderer, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&sink), "sink");
    check(IBaseFilter_QueryInterface(grabber, &IID_ISampleGrabber, (void **)&samples), "samples");
    type.majortype = MEDIATYPE_Video; type.subtype = MEDIASUBTYPE_RGB24; type.formattype = FORMAT_VideoInfo;
    check(ISampleGrabber_SetMediaType(samples, &type), "RGB24");
    check(ISampleGrabber_SetCallback(samples, &callback, 0), "callback");
    check(IGraphBuilder_AddFilter(graph, grabber, L"samples"), "add grabber");
    check(IGraphBuilder_AddFilter(graph, sink, L"sink"), "add sink");
    out = pin(grabber, PINDIR_OUTPUT); in = pin(sink, PINDIR_INPUT);
    check(IGraphBuilder_Connect(graph, out, in), "sink connection");
    check(IGraphBuilder_RenderFile(graph, file, NULL), "render");
    check(IGraphBuilder_QueryInterface(graph, &IID_IMediaControl, (void **)&control), "control");
    check(IGraphBuilder_QueryInterface(graph, &IID_IMediaEvent, (void **)&event), "event");
    frames = 0; last = -1;
    check(IMediaControl_Run(control), "run");
    check(IMediaEvent_WaitForCompletion(event, 10000, &code), "complete");
    check(IMediaControl_Stop(control), "stop");
    printf("dshow frames=%u last=%I64d code=%ld\n", frames, last, code);
    if (frames != 5 || code != EC_COMPLETE) ExitProcess(3);
    IMediaEvent_Release(event); IMediaControl_Release(control);
    IPin_Release(in); IPin_Release(out);
    ISampleGrabber_Release(samples); IBaseFilter_Release(grabber); IBaseFilter_Release(sink); IGraphBuilder_Release(graph);
}
int main(void) { CoInitialize(NULL); run(L"synthetic.mpg"); run(L"synthetic.avi"); CoUninitialize(); puts("media dshow ok"); return 0; }
