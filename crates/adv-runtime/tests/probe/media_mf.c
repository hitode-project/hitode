/* i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 media_mf.c -o media_mf.exe -lmfreadwrite -lmfplat -lmfuuid -lole32 -luuid */
#define COBJMACROS
#include <windows.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <stdio.h>
static void check(HRESULT hr, const char *where) { if (FAILED(hr)) { printf("FAIL %s %08lx\n", where, hr); ExitProcess(1); } }
int main(void)
{
    IMFSourceReader *reader;
    IMFMediaType *type;
    unsigned count = 0;
    LONGLONG last = -1;
    CoInitialize(NULL); check(MFStartup(MF_VERSION, MFSTARTUP_FULL), "startup");
    check(MFCreateSourceReaderFromURL(L"synthetic.mp4", NULL, &reader), "reader");
    check(MFCreateMediaType(&type), "type");
    check(IMFMediaType_SetGUID(type, &MF_MT_MAJOR_TYPE, &MFMediaType_Video), "major");
    check(IMFMediaType_SetGUID(type, &MF_MT_SUBTYPE, &MFVideoFormat_NV12), "subtype");
    check(IMFSourceReader_SetCurrentMediaType(reader, MF_SOURCE_READER_FIRST_VIDEO_STREAM, NULL, type), "set type");
    IMFMediaType_Release(type);
    for (;;) {
        IMFSample *sample = NULL;
        DWORD stream, flags;
        LONGLONG time;
        check(IMFSourceReader_ReadSample(reader, MF_SOURCE_READER_FIRST_VIDEO_STREAM, 0, &stream, &flags, &time, &sample), "sample");
        if (flags & MF_SOURCE_READERF_ENDOFSTREAM) break;
        if (sample) {
            IMFMediaBuffer *buffer;
            BYTE *data;
            DWORD size;
            if (time < last) return 2;
            last = time; count++;
            check(IMFSample_ConvertToContiguousBuffer(sample, &buffer), "buffer");
            check(IMFMediaBuffer_Lock(buffer, &data, NULL, &size), "lock");
            if (size != 64 * 48 * 3 / 2 || data[0] < 75 || data[0] > 90) return 3;
            IMFMediaBuffer_Unlock(buffer); IMFMediaBuffer_Release(buffer); IMFSample_Release(sample);
        }
    }
    printf("mf frames=%u last=%I64d\n", count, last);
    IMFSourceReader_Release(reader); MFShutdown(); CoUninitialize();
    if (count != 5 || last != 4000000) return 4;
    puts("media mf ok"); return 0;
}
