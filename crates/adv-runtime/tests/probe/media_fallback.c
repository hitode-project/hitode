/* i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 media_fallback.c -o media_fallback.exe -lstrmiids -lole32 -luuid */
#define COBJMACROS
#include <windows.h>
#include <dshow.h>
#include <d3d9.h>
#include <vmr9.h>
#include <stdio.h>
int main(void)
{
    IBaseFilter *filter;
    IVMRFilterConfig9 *config;
    DWORD mode;
    CoInitialize(NULL);
    HRESULT hr = CoCreateInstance(&CLSID_VideoMixingRenderer9, NULL, CLSCTX_INPROC_SERVER, &IID_IBaseFilter, (void **)&filter);
    if (FAILED(hr)) { printf("FAIL fallback %08lx\n", hr); return 1; }
    hr = IBaseFilter_QueryInterface(filter, &IID_IVMRFilterConfig9, (void **)&config);
    if (FAILED(hr)) return 2;
    if (FAILED(IVMRFilterConfig9_GetRenderingMode(config, &mode)) || mode != VMR9Mode_Windowed) return 3;
    if (FAILED(IVMRFilterConfig9_SetRenderingMode(config, VMR9Mode_Renderless))) return 4;
    if (FAILED(IVMRFilterConfig9_GetRenderingMode(config, &mode)) || mode != VMR9Mode_Renderless) return 5;
    IVMRFilterConfig9_Release(config); IBaseFilter_Release(filter);
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
