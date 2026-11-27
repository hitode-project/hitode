/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 -municode media_amstream.c -o media_amstream.exe -lddraw -lstrmiids -lole32 -luuid */
#define COBJMACROS
#include <windows.h>
#include <ddraw.h>
#include <dshow.h>
#include <initguid.h>
#include <amstream.h>
#include <ddstream.h>
#include <mmreg.h>
#include <stdio.h>

static void require(int condition, const char *where, HRESULT hr)
{
    if (!condition) {
        printf("FAIL %s %08lx\n", where, hr);
        fflush(stdout);
        ExitProcess(1);
    }
}

#define CHECK(call) do { HRESULT hr = (call); require(hr == S_OK, #call, hr); } while (0)

static void check_audio(IAMMultiMediaStream *stream)
{
    IGraphBuilder *graph;
    IEnumFilters *filters;
    IBaseFilter *filter;
    unsigned audio = 0;
    unsigned mpeg = 0;

    CHECK(IAMMultiMediaStream_GetFilterGraph(stream, &graph));
    /* Titles look the MPEG-1 decoders up by the names Windows gives them. */
    CHECK(IGraphBuilder_FindFilterByName(graph, L"MPEG Video Decoder", &filter));
    IBaseFilter_Release(filter);
    CHECK(IGraphBuilder_FindFilterByName(graph, L"MPEG Audio Decoder", &filter));
    IBaseFilter_Release(filter);
    CHECK(IGraphBuilder_EnumFilters(graph, &filters));
    while (IEnumFilters_Next(filters, 1, &filter, NULL) == S_OK) {
        CLSID clsid;
        CHECK(IBaseFilter_GetClassID(filter, &clsid));
        if (IsEqualGUID(&clsid, &CLSID_MPEG1Splitter)) {
            IPin *pin;
            AM_MEDIA_TYPE type;
            CHECK(IBaseFilter_FindPin(filter, L"Audio", &pin));
            CHECK(IPin_ConnectionMediaType(pin, &type));
            require(IsEqualGUID(&type.formattype, &FORMAT_WaveFormatEx) &&
                    type.pbFormat && type.cbFormat == sizeof(MPEG1WAVEFORMAT),
                    "MPEG audio media type", 0);
            MPEG1WAVEFORMAT *wave = (MPEG1WAVEFORMAT *)type.pbFormat;
            require(wave->wfx.wFormatTag == WAVE_FORMAT_MPEG && wave->wfx.cbSize == 22 &&
                    wave->fwHeadLayer == ACM_MPEG_LAYER2 && wave->wfx.nBlockAlign &&
                    wave->wfx.nAvgBytesPerSec == wave->dwHeadBitrate / 8,
                    "MPEG audio format", 0);
            CoTaskMemFree(type.pbFormat);
            if (type.pUnk) IUnknown_Release(type.pUnk);
            IPin_Release(pin);
            mpeg++;
        }
        if (IsEqualGUID(&clsid, &CLSID_DSoundRender)) {
            IEnumPins *pins;
            IPin *pin;
            AM_MEDIA_TYPE type;
            WAVEFORMATEX *wave;
            CHECK(IBaseFilter_EnumPins(filter, &pins));
            CHECK(IEnumPins_Next(pins, 1, &pin, NULL));
            CHECK(IPin_ConnectionMediaType(pin, &type));
            require(IsEqualGUID(&type.majortype, &MEDIATYPE_Audio) &&
                    IsEqualGUID(&type.subtype, &MEDIASUBTYPE_PCM) &&
                    IsEqualGUID(&type.formattype, &FORMAT_WaveFormatEx) &&
                    type.pbFormat && type.cbFormat >= sizeof(WAVEFORMATEX),
                    "audio media type", 0);
            wave = (WAVEFORMATEX *)type.pbFormat;
            require(wave->wFormatTag == WAVE_FORMAT_PCM && wave->nChannels &&
                    wave->nSamplesPerSec && wave->wBitsPerSample == 16 &&
                    wave->nBlockAlign == wave->nChannels * 2 &&
                    wave->nAvgBytesPerSec == wave->nSamplesPerSec * wave->nBlockAlign,
                    "PCM format", 0);
            printf("PCM %lu Hz, %u channels\n", wave->nSamplesPerSec, wave->nChannels);
            CoTaskMemFree(type.pbFormat);
            if (type.pUnk) IUnknown_Release(type.pUnk);
            IPin_Release(pin);
            IEnumPins_Release(pins);
            audio++;
        }
        IBaseFilter_Release(filter);
    }
    require(mpeg == 1, "MPEG audio connected", mpeg);
    require(audio == 1, "audio renderer connected", audio);
    IEnumFilters_Release(filters);
    IGraphBuilder_Release(graph);
}

int wmain(int argc, WCHAR **argv)
{
    IAMMultiMediaStream *stream;
    IDirectDraw *draw;
    IMediaStream *video;
    IDirectDrawMediaStream *ddstream;
    IDirectDrawStreamSample *sample;
    IDirectDrawSurface *surface;
    unsigned frames = 0;
    DWORD pixels = 0;

    CHECK(CoInitialize(NULL));
    CHECK(DirectDrawCreate(NULL, &draw, NULL));
    CHECK(IDirectDraw_SetCooperativeLevel(draw, NULL, DDSCL_NORMAL));
    CHECK(CoCreateInstance(&CLSID_AMMultiMediaStream, NULL, CLSCTX_INPROC_SERVER,
                          &IID_IAMMultiMediaStream, (void **)&stream));
    CHECK(IAMMultiMediaStream_Initialize(stream, STREAMTYPE_READ, 0, NULL));
    CHECK(IAMMultiMediaStream_AddMediaStream(stream, (IUnknown *)draw, &MSPID_PrimaryVideo, 0, &video));
    CHECK(IAMMultiMediaStream_AddMediaStream(stream, NULL, &MSPID_PrimaryAudio, AMMSF_ADDDEFAULTRENDERER, NULL));
    CHECK(IAMMultiMediaStream_OpenFile(stream, argc > 1 ? argv[1] : L"synthetic.mpg", 0));
    check_audio(stream);
    CHECK(IMediaStream_QueryInterface(video, &IID_IDirectDrawMediaStream, (void **)&ddstream));
    CHECK(IDirectDrawMediaStream_CreateSample(ddstream, NULL, NULL, 0, &sample));
    CHECK(IDirectDrawStreamSample_GetSurface(sample, &surface, NULL));

    CHECK(IAMMultiMediaStream_SetState(stream, STREAMSTATE_RUN));
    while (frames < 5) {
        DDSURFACEDESC desc = {sizeof(desc)};
        HRESULT hr = IDirectDrawStreamSample_Update(sample, 0, NULL, NULL, 0);
        if (hr == MS_S_ENDOFSTREAM) break;
        require(hr == S_OK, "surface update", hr);
        CHECK(IDirectDrawSurface_Lock(surface, NULL, &desc, DDLOCK_WAIT | DDLOCK_READONLY, NULL));
        const BYTE *pixel = desc.lpSurface;
        for (DWORD y = 0; y < desc.dwHeight; y++) {
            const BYTE *row = pixel + y * desc.lPitch;
            for (DWORD x = 0; x < desc.dwWidth * desc.ddpfPixelFormat.dwRGBBitCount / 8; x++)
                pixels = pixels * 31 + row[x];
        }
        require(argc > 1 || (desc.ddpfPixelFormat.dwRGBBitCount == 32 &&
                           pixel[0] < 12 && pixel[1] < 12 && pixel[2] > 230),
                "decoded surface pixel", 0);
        CHECK(IDirectDrawSurface_Unlock(surface, NULL));
        frames++;
    }
    require(frames > 0, "decoded surface frames", frames);
    CHECK(IAMMultiMediaStream_SetState(stream, STREAMSTATE_STOP));
    IDirectDrawSurface_Release(surface);
    IDirectDrawStreamSample_Release(sample);
    IDirectDrawMediaStream_Release(ddstream);
    IMediaStream_Release(video);
    IAMMultiMediaStream_Release(stream);
    IDirectDraw_Release(draw);
    CoUninitialize();
    printf("media amstream ok, %u surface frames, pixel checksum %08lx\n", frames, pixels);
    return 0;
}
