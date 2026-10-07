/* Authored x86 ImageList stream oracle. All images are generated solid colors. */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <objidl.h>
#include <commctrl.h>

static BYTE data[131072];
static DWORD data_size;
static DWORD failures;

static void bytes_copy(void *to, const void *from, DWORD length) {
    BYTE *destination = (BYTE *)to;
    const BYTE *source = (const BYTE *)from;
    while (length--) *destination++ = *source++;
}

static void record(const char *name, DWORD a, DWORD b, DWORD c, DWORD d) {
    char line[240]; DWORD n = 0, written = 0, index, shift;
    DWORD values[4] = {a, b, c, d};
    while (*name && n < 190) line[n++] = *name++;
    for (index = 0; index < 4; ++index) {
        line[n++] = ' ';
        for (shift = 32; shift; shift -= 4)
            line[n++] = "0123456789abcdef"[(values[index] >> (shift - 4)) & 15];
    }
    line[n++] = '\r'; line[n++] = '\n';
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), line, n, &written, NULL);
}

static DWORD word(DWORD offset) {
    return data[offset] | ((DWORD)data[offset + 1] << 8);
}
static DWORD dword(DWORD offset) {
    return word(offset) | (word(offset + 2) << 16);
}
static void put_word(DWORD offset, DWORD value) {
    data[offset] = (BYTE)value; data[offset + 1] = (BYTE)(value >> 8);
}

typedef struct {
    IStream stream;
    DWORD position, calls, count_outputs, max_piece, mode;
    DWORD requested[16], actual[16];
} ProbeStream;

static HRESULT WINAPI query(IStream *stream, REFIID iid, void **object) {
    (void)stream; (void)iid;
    if (object) *object = NULL;
    return E_NOINTERFACE;
}
static ULONG WINAPI addref(IStream *stream) { (void)stream; return 2; }
static ULONG WINAPI release(IStream *stream) { (void)stream; return 1; }
static HRESULT WINAPI read_stream(IStream *stream, void *buffer, ULONG count, ULONG *read) {
    ProbeStream *probe = (ProbeStream *)stream;
    DWORD got = data_size - probe->position;
    DWORD call = probe->calls++;
    if (got > count) got = count;
    if (probe->max_piece && got > probe->max_piece) got = probe->max_piece;
    if (probe->mode == 3 || (probe->mode == 7 && call == 8)) got = 0;
    if (call < 16) { probe->requested[call] = count; probe->actual[call] = got; }
    bytes_copy(buffer, data + probe->position, got);
    probe->position += got;
    if (read) {
        probe->count_outputs++;
        if (probe->mode != 6) *read = probe->mode == 5 ? count + 1 : got;
    }
    if (probe->mode == 3 || (probe->mode == 7 && call == 8)) return E_FAIL;
    if (probe->mode == 2 || probe->mode == 4) return S_FALSE;
    return got == count || probe->mode == 1 ? S_OK : S_FALSE;
}
static HRESULT WINAPI write_stream(IStream *stream, const void *buffer, ULONG count, ULONG *written) {
    (void)stream; (void)buffer; (void)count; (void)written; return E_NOTIMPL;
}
static HRESULT WINAPI seek_stream(IStream *stream, LARGE_INTEGER offset, DWORD origin, ULARGE_INTEGER *position) {
    (void)stream; (void)offset; (void)origin; (void)position; return E_NOTIMPL;
}
static HRESULT WINAPI set_size(IStream *stream, ULARGE_INTEGER size) { (void)stream; (void)size; return E_NOTIMPL; }
static HRESULT WINAPI copy_to(IStream *stream, IStream *to, ULARGE_INTEGER count, ULARGE_INTEGER *read, ULARGE_INTEGER *written) {
    (void)stream; (void)to; (void)count; (void)read; (void)written; return E_NOTIMPL;
}
static HRESULT WINAPI commit(IStream *stream, DWORD flags) { (void)stream; (void)flags; return E_NOTIMPL; }
static HRESULT WINAPI revert(IStream *stream) { (void)stream; return E_NOTIMPL; }
static HRESULT WINAPI lock_region(IStream *stream, ULARGE_INTEGER offset, ULARGE_INTEGER count, DWORD flags) {
    (void)stream; (void)offset; (void)count; (void)flags; return E_NOTIMPL;
}
static HRESULT WINAPI unlock_region(IStream *stream, ULARGE_INTEGER offset, ULARGE_INTEGER count, DWORD flags) {
    (void)stream; (void)offset; (void)count; (void)flags; return E_NOTIMPL;
}
static HRESULT WINAPI stat_stream(IStream *stream, STATSTG *stat, DWORD flags) {
    (void)stream; (void)stat; (void)flags; return E_NOTIMPL;
}
static HRESULT WINAPI clone_stream(IStream *stream, IStream **clone) {
    (void)stream; (void)clone; return E_NOTIMPL;
}
static IStreamVtbl stream_vtable = {
    query, addref, release, read_stream, write_stream, seek_stream, set_size,
    copy_to, commit, revert, lock_region, unlock_region, stat_stream, clone_stream
};

static void probe_read(const char *name, DWORD mode, DWORD max_piece, int expected) {
    ProbeStream probe = {0};
    HIMAGELIST list = NULL;
    DWORD exception = 0, i;
    int cx = 0, cy = 0;
    probe.stream.lpVtbl = &stream_vtable;
    probe.mode = mode; probe.max_piece = max_piece;
    __try { list = ImageList_Read(&probe.stream); }
    __except(EXCEPTION_EXECUTE_HANDLER) { exception = GetExceptionCode(); }
    record(name, list != NULL, probe.position, probe.calls, exception);
    if (expected >= 0 && ((list != NULL) != expected || exception || probe.count_outputs)) ++failures;
    record("Read outputs/count/size", probe.count_outputs, list ? ImageList_GetImageCount(list) : 0, 0, 0);
    if (list) {
        IMAGEINFO image;
        BITMAP bitmap;
        BITMAPINFO info = {0};
        DWORD pixels[128] = {0};
        HDC dc;
        ImageList_GetIconSize(list, &cx, &cy);
        record("Read icon dimensions", cx, cy, 0, 0);
        if (ImageList_GetImageInfo(list, 0, &image) && GetObjectA(image.hbmImage, sizeof(bitmap), &bitmap)
            && bitmap.bmWidth * bitmap.bmHeight <= 128) {
            info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
            info.bmiHeader.biWidth = bitmap.bmWidth; info.bmiHeader.biHeight = -bitmap.bmHeight;
            info.bmiHeader.biPlanes = 1; info.bmiHeader.biBitCount = 32;
            dc = GetDC(NULL);
            if (GetDIBits(dc, image.hbmImage, 0, bitmap.bmHeight, pixels, &info, DIB_RGB_COLORS))
                record("Read first image pixel/bpp", pixels[0], bitmap.bmBitsPixel, 0, 0);
            ReleaseDC(NULL, dc);
        }
        ImageList_Destroy(list);
    }
    for (i = 0; i < probe.calls && i < 16; ++i)
        record("Read request/actual", i, probe.requested[i], probe.actual[i], 0);
}

static BOOL make_stream(UINT flags, const char *filename) {
    HIMAGELIST list;
    HBITMAP bitmap;
    BITMAPINFO info = {0};
    DWORD *pixels;
    IStream *stream = NULL;
    HGLOBAL memory;
    STATSTG stat = {0};
    HANDLE file;
    DWORD written, i;
    BOOL result = FALSE;
    list = ImageList_Create(2, 3, flags, 5, 1);
    if (!list) return FALSE;
    info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    info.bmiHeader.biWidth = 10; info.bmiHeader.biHeight = -3;
    info.bmiHeader.biPlanes = 1; info.bmiHeader.biBitCount = 32;
    bitmap = CreateDIBSection(NULL, &info, DIB_RGB_COLORS, (void **)&pixels, NULL, 0);
    if (!bitmap) goto cleanup;
    for (i = 0; i < 30; ++i)
        pixels[i] = 0xff000000 | (((i % 10) / 2 + 1) * 0x202020) | ((i / 10) * 0x030507);
    if (ImageList_Add(list, bitmap, NULL) != 0) goto cleanup_bitmap;
    if (FAILED(CreateStreamOnHGlobal(NULL, TRUE, &stream))) goto cleanup_bitmap;
    if (!ImageList_Write(list, stream)) goto cleanup_stream;
    if (FAILED(IStream_Stat(stream, &stat, STATFLAG_NONAME)) || stat.cbSize.HighPart || stat.cbSize.LowPart > sizeof(data))
        goto cleanup_stream;
    data_size = stat.cbSize.LowPart;
    if (FAILED(GetHGlobalFromStream(stream, &memory))) goto cleanup_stream;
    bytes_copy(data, GlobalLock(memory), data_size); GlobalUnlock(memory);
    record("IL header magic/version/count/capacity", word(0), word(2), word(4), word(6));
    record("IL grow/width/height/flags", word(8), word(10), word(12), word(18));
    record("BMP width/height/bpp/streambytes", dword(46), dword(50), word(56), data_size);
    record("BMP size/offset/pixels/compression", dword(30), dword(38), dword(62), dword(58));
    file = CreateFileA(filename, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (file == INVALID_HANDLE_VALUE) goto cleanup_stream;
    result = WriteFile(file, data, data_size, &written, NULL) && written == data_size;
    CloseHandle(file);
cleanup_stream:
    IStream_Release(stream);
cleanup_bitmap:
    DeleteObject(bitmap);
cleanup:
    ImageList_Destroy(list);
    return result;
}

void mainCRTStartup(void) {
    DWORD size, saved, flags, i, pixels, row, expected;
    InitCommonControls();
    if (!make_stream(ILC_COLOR32 | ILC_MASK, "imagelist-color32-mask.bin")) ExitProcess(1);
    /* Native streams store four columns and bottom-up bitmap scanlines. */
    pixels = 28 + dword(38);
    for (i = 0; i < 5; ++i) {
        row = dword(50) - 1 - (i / 4) * 3;
        expected = 0xff000000 | ((i + 1) * 0x202020);
        saved = dword(pixels + (row * dword(46) + (i % 4) * 2) * 4);
        record("Atlas index/pixel/expected/match", i, saved, expected, saved == expected);
        if (saved != expected) ++failures;
    }
    probe_read("Read normal", 0, 0, 1);
    probe_read("Read S_OK partial-seven", 1, 7, 0);
    probe_read("Read S_FALSE partial-seven", 2, 7, 0);
    probe_read("Read E_FAIL", 3, 0, 0);
    probe_read("Read S_FALSE full", 4, 0, 1);
    probe_read("Read overreported pcbRead", 5, 0, 1);
    probe_read("Read unchanged pcbRead", 6, 0, 1);
    size = data_size; --data_size;
    probe_read("Read truncated final-byte", 0, 0, -1);
    data_size = size;
    saved = word(2); put_word(2, 0x0100); probe_read("Read unknown version", 0, 0, 0); put_word(2, saved);
    flags = word(18);
    put_word(18, 0x10ff); probe_read("Read internal flags 0x10ff", 0, 0, 1);
    probe_read("Read extended tail E_FAIL", 7, 0, 0);
    put_word(18, 0x00fe); probe_read("Read DDB flags 0xfe", 0, 0, 1);
    put_word(18, flags);
    saved = data[pixels + ((dword(50) - 1) * dword(46)) * 4 + 3];
    data[pixels + ((dword(50) - 1) * dword(46)) * 4 + 3] = 128;
    probe_read("Read COLOR32 alpha-128", 0, 0, 1);
    put_word(18, 0x00fe); probe_read("Read DDB alpha-128", 0, 0, 1);
    put_word(18, flags);
    data[pixels + ((dword(50) - 1) * dword(46)) * 4 + 3] = (BYTE)saved;
    if (!make_stream(ILC_COLOR24 | ILC_MASK, "imagelist-color24-mask.bin")) ++failures;
    probe_read("Read COLOR24", 0, 0, 1);
    if (!make_stream(ILC_COLOR8 | ILC_MASK, "imagelist-color8-mask.bin")) ++failures;
    probe_read("Read COLOR8", 0, 0, 1);
    if (!make_stream(ILC_COLOR4 | ILC_MASK, "imagelist-color4-mask.bin")) ++failures;
    probe_read("Read COLOR4", 0, 0, 1);
    if (!make_stream(ILC_COLOR32, "imagelist-color32.bin")) ++failures;
    probe_read("Read COLOR32 no mask", 0, 0, 1);
    ExitProcess(failures ? 2 : 0);
}
