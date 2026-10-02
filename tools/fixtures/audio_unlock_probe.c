/* Native Windows behavior probe; never plays sound and does not call the ADV runtime. */
#include "dsound_min.h"
API HWND WINAPI GetDesktopWindow(void);
API HANDLE WINAPI GetStdHandle(DWORD);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
static void result(const char *name, HRESULT code) {
    static const char digits[] = "0123456789abcdef";
    char text[128];
    DWORD i = 0, j, written;
    while (*name) text[i++] = *name++;
    text[i++] = '='; text[i++] = '0'; text[i++] = 'x';
    for (j = 0; j < 8; ++j) text[i++] = digits[((DWORD)code >> (28 - j * 4)) & 15];
    text[i++] = '\r'; text[i++] = '\n';
    WriteFile(GetStdHandle((DWORD)-11), text, i, &written, NULL);
}
void mainCRTStartup(void) {
    WAVEFORMATEX format = {1, 1, 44100, 88200, 2, 16, 0};
    DSBUFFERDESC description = {0};
    void *device = NULL;
    HRESULT hr;
    int variant;
    description.size = sizeof(description); description.flags = 8;
    description.bytes = 1024; description.format = &format;
    hr = DirectSoundCreate(NULL, &device, NULL); result("DirectSoundCreate", hr);
    if (hr) ExitProcess(1);
    hr = DS_METHOD(DsCooperate, device, 6)(device, GetDesktopWindow(), 1);
    result("SetCooperativeLevel", hr);
    if (hr) ExitProcess(2);
    for (variant = 0; variant < 4; ++variant) {
        const char *label;
        void *buffer = NULL, *argument;
        /* Keep both candidate source regions mapped even if a driver copies them. */
        struct { void *pointer; DWORD bytes; BYTE padding[128]; } output = {0};
        BYTE arbitrary[128] = {0};
        hr = DS_METHOD(DsCreateBuffer, device, 3)(device, &description, &buffer, NULL);
        result("CreateSoundBuffer", hr);
        if (hr) ExitProcess(3);
        hr = DS_METHOD(DsLock, buffer, 11)(buffer, 0, 64, &output.pointer, &output.bytes, NULL, NULL, 0);
        result("Lock", hr);
        if (hr) ExitProcess(4);
        if (variant == 0) { label = "Unlock.correct_pointer"; argument = output.pointer; }
        else if (variant == 1) { label = "Unlock.output_parameter_address"; argument = &output.pointer; }
        else if (variant == 2) { label = "Unlock.arbitrary_valid_memory"; argument = arbitrary; }
        else { label = "Unlock.null_pointer"; argument = NULL; }
        hr = DS_METHOD(DsUnlock, buffer, 19)(buffer, argument, output.bytes, NULL, 0);
        result(label, hr);
        if (variant == 0) {
            hr = DS_METHOD(DsUnlock, buffer, 19)(buffer, argument, output.bytes, NULL, 0);
            result("Unlock.repeat_after_correct", hr);
        } else {
            void *next_pointer = NULL;
            DWORD next_bytes = 0;
            hr = DS_METHOD(DsLock, buffer, 11)(buffer, 0, 64, &next_pointer, &next_bytes, NULL, NULL, 0);
            result("Lock.after_invalid_unlock", hr);
            if (!hr) {
                hr = DS_METHOD(DsUnlock, buffer, 19)(buffer, next_pointer, next_bytes, NULL, 0);
                result("Unlock.after_repeated_lock", hr);
            }
        }
        DS_METHOD(DsRelease, buffer, 2)(buffer);
    }
    {
        void *buffer = NULL, *pointer = NULL;
        DWORD bytes = 0, status = 0, i;
        hr = DS_METHOD(DsCreateBuffer, device, 3)(device, &description, &buffer, NULL);
        result("CreateSoundBuffer.play_locked", hr);
        if (hr) ExitProcess(5);
        hr = DS_METHOD(DsLock, buffer, 11)(buffer, 0, 1024, &pointer, &bytes, NULL, NULL, 0);
        result("Lock.before_play", hr);
        if (hr) ExitProcess(6);
        for (i = 0; i < bytes; ++i) ((BYTE *)pointer)[i] = 0;
        hr = DS_METHOD(DsPlay, buffer, 12)(buffer, 0, 0, 1);
        result("Play.while_locked", hr);
        hr = DS_METHOD(DsStatus, buffer, 9)(buffer, &status);
        result("GetStatus.while_locked", hr);
        result("GetStatus.value", (HRESULT)status);
        Sleep(20);
        hr = DS_METHOD(DsPosition, buffer, 4)(buffer, &status, NULL);
        result("GetCurrentPosition.while_locked", hr);
        result("GetCurrentPosition.value", (HRESULT)status);
        hr = DS_METHOD(DsStop, buffer, 18)(buffer);
        result("Stop.while_locked", hr);
        hr = DS_METHOD(DsUnlock, buffer, 19)(buffer, pointer, bytes, NULL, 0);
        result("Unlock.after_locked_play", hr);
        DS_METHOD(DsRelease, buffer, 2)(buffer);
    }
    DS_METHOD(DsRelease, device, 2)(device);
    ExitProcess(0);
}
