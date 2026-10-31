/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 media_wm.c -o media_wm.exe -lole32 -luuid */
#define COBJMACROS
#include <windows.h>
#include <wmsdk.h>
#include <nserror.h>
#include <stdio.h>
static void check(HRESULT hr, const char *where) { if (FAILED(hr)) { printf("FAIL %s %08lx\n", where, hr); ExitProcess(1); } }
int main(void)
{
    IWMSyncReader *reader;
    DWORD outputs, i, count[2];
    QWORD last[2];
    int compressed;
    CoInitialize(NULL);
    typedef HRESULT (WINAPI *CreateReader)(IUnknown *, DWORD, IWMSyncReader **);
    HMODULE module = LoadLibraryW(L"wmvcore.dll");
    CreateReader create = (CreateReader)GetProcAddress(module, "WMCreateSyncReader");
    if (!create) return 5;
    check(create(NULL, 0, &reader), "create");
    check(IWMSyncReader_Open(reader, L"synthetic.wmv"), "open");
    check(IWMSyncReader_GetOutputCount(reader, &outputs), "outputs");
    if (outputs != 2) return 2;
    for (compressed = 0; compressed < 2; compressed++) {
        for (i = 1; i <= outputs; i++) check(IWMSyncReader_SetReadStreamSamples(reader, i, compressed), "compressed");
        check(IWMSyncReader_SetRange(reader, 0, 0), "range");
        ZeroMemory(count, sizeof(count)); ZeroMemory(last, sizeof(last));
        for (;;) {
            INSSBuffer *sample = NULL;
            QWORD time, duration;
            DWORD flags, output, size;
            WORD stream;
            BYTE *data;
            HRESULT hr = IWMSyncReader_GetNextSample(reader, 0, &sample, &time, &duration, &flags, &output, &stream);
            if (hr == NS_E_NO_MORE_SAMPLES) break;
            check(hr, "sample");
            check(INSSBuffer_GetBufferAndLength(sample, &data, &size), "buffer");
            if (output >= 2 || !size || (count[output] && time < last[output])) return 3;
            last[output] = time; count[output]++;
            if (!compressed && output == 0 && (data[0] > 12 || data[1] > 12 || data[2] < 230)) return 6;
            if (!compressed && output == 0 && count[output] == 1) printf("wm pixel=%u,%u,%u size=%lu\n", data[0], data[1], data[2], size);
            INSSBuffer_Release(sample);
        }
        printf("wm compressed=%d samples=%lu,%lu last=%I64u,%I64u\n", compressed, count[0], count[1], last[0], last[1]);
        if (count[0] != 5 || count[1] != (compressed ? 11 : 10) || last[0] != 4460000) return 4;
    }
    check(IWMSyncReader_Close(reader), "close");
    IWMSyncReader_Release(reader); CoUninitialize();
    puts("media wm ok"); return 0;
}
