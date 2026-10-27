/* i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 media_fallback.c -o media_fallback.exe -lstrmiids -lole32 -luuid */
#define COBJMACROS
#include <windows.h>
#include <dshow.h>
#include <d3d9.h>
#include <vmr9.h>
#include <initguid.h>
#include <wmcodecdsp.h>
#include <dmodshow.h>
#include <dmoreg.h>
#include <stdio.h>
int main(void)
{
    IBaseFilter *filter;
    IVMRFilterConfig9 *config;
    DWORD mode;
    CoInitialize(NULL);
    IUnknown *decoder;
    HRESULT hr = CoCreateInstance(&CLSID_CWMVDecMediaObject, NULL, CLSCTX_INPROC_SERVER, &IID_IUnknown, (void **)&decoder);
    if (FAILED(hr)) { printf("FAIL decoder capability %08lx\n", hr); return 8; }
    IUnknown_Release(decoder);
    hr = CoCreateInstance(&CLSID_VideoMixingRenderer9, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&filter);
    if (FAILED(hr)) { printf("FAIL fallback %08lx\n", hr); return 1; }
    hr = IBaseFilter_QueryInterface(filter, &IID_IVMRFilterConfig9, (void **)&config);
    if (FAILED(hr)) return 2;
    if (FAILED(IVMRFilterConfig9_GetRenderingMode(config, &mode)) || mode != VMR9Mode_Windowed) return 3;
    if (FAILED(IVMRFilterConfig9_SetRenderingMode(config, VMR9Mode_Renderless))) return 4;
    if (FAILED(IVMRFilterConfig9_GetRenderingMode(config, &mode)) || mode != VMR9Mode_Renderless) return 5;
    IVMRFilterConfig9_Release(config); IBaseFilter_Release(filter);
    /* External graphs push into Wine's renderer with Wine-owned samples. */
    IEnumPins *pins;
    IPin *pin;
    IMemInputPin *input;
    IMemAllocator *allocator, *queried;
    if (FAILED(CoCreateInstance(&CLSID_DSoundRender, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&filter))) return 9;
    if (FAILED(IBaseFilter_EnumPins(filter, &pins)) || IEnumPins_Next(pins, 1, &pin, NULL) != S_OK) return 10;
    if (FAILED(IPin_QueryInterface(pin, &IID_IMemInputPin, (void **)&input))) return 11;
    if (FAILED(CoCreateInstance(&CLSID_MemoryAllocator, NULL, CLSCTX_INPROC_SERVER, &IID_IMemAllocator, (void **)&allocator))) return 12;
    if (FAILED(IMemInputPin_NotifyAllocator(input, allocator, FALSE))) return 13;
    if (FAILED(IMemInputPin_GetAllocator(input, &queried)) || queried != allocator) return 14;
    IMemAllocator_Release(queried); IMemAllocator_Release(allocator);
    IMemInputPin_Release(input); IPin_Release(pin); IEnumPins_Release(pins); IBaseFilter_Release(filter);
    IDMOWrapperFilter *wrapper;
    if (FAILED(CoCreateInstance(&CLSID_DMOWrapperFilter, NULL, CLSCTX_INPROC_SERVER, &IID_IDMOWrapperFilter, (void **)&wrapper))) return 15;
    if (FAILED(IDMOWrapperFilter_Init(wrapper, &CLSID_CWMVDecMediaObject, &DMOCATEGORY_VIDEO_DECODER))) return 16;
    if (FAILED(IDMOWrapperFilter_QueryInterface(wrapper, &IID_IBaseFilter, (void **)&filter))) return 17;
    if (FAILED(IBaseFilter_EnumPins(filter, &pins)) || IEnumPins_Next(pins, 1, &pin, NULL) != S_OK) return 18;
    if (FAILED(IPin_QueryInterface(pin, &IID_IMemInputPin, (void **)&input))) return 19;
    if (FAILED(IMemInputPin_GetAllocator(input, &allocator))) return 20;
    IMemAllocator_Release(allocator); IMemInputPin_Release(input);
    IPin_Release(pin); IEnumPins_Release(pins); IBaseFilter_Release(filter);
    IDMOWrapperFilter_Release(wrapper);
    /* Exercise a forwarded export from each image, by ordinal as well as name. */
    typedef HRESULT (WINAPI *CheckURL)(const WCHAR *);
    HMODULE wm = LoadLibraryW(L"wmvcore.dll");
    CheckURL check = (CheckURL)GetProcAddress(wm, (const char *)1);
    if (!check || check(NULL) != E_INVALIDARG) return 6;
    typedef long (WINAPI *ErrorText)(HRESULT, WCHAR *, DWORD);
    ErrorText text = (ErrorText)GetProcAddress(GetModuleHandleW(L"quartz.dll"), (const char *)2);
    WCHAR buffer[256];
    if (!text || !text(E_FAIL, buffer, 256) || !buffer[0]) return 7;
    CoUninitialize(); puts("media fallback ok"); return 0;
}
