/* Real guest DirectSound calls and low-amplitude mono PCM; no CRT or game assets. */
#include "dsound_min.h"
static volatile DWORD work;
static DWORD WINAPI compute_worker(void *unused) {
    (void)unused;
    for (;;) ++work;
}
void mainCRTStartup(void) {
    WAVEFORMATEX format = {1, 1, 44100, 88200, 2, 16, 0};
    DSBUFFERDESC description = {0};
    void *device = NULL, *buffer = NULL, *first = NULL, *second = NULL;
    DWORD first_bytes = 0, second_bytes = 0, position = 0, status = 0;
    DWORD i;
    description.size = sizeof(description); description.flags = 8;
    description.bytes = 88200; description.format = &format;
    if (DirectSoundCreate(NULL, &device, NULL)) ExitProcess(60);
    if (DS_METHOD(DsCreateBuffer, device, 3)(device, &description, &buffer, NULL)) ExitProcess(61);
    if (DS_METHOD(DsLock, buffer, 11)(buffer, 0, description.bytes,
        &first, &first_bytes, &second, &second_bytes, 0)) ExitProcess(62);
    if (first_bytes != description.bytes || second_bytes) ExitProcess(63);
    for (i = 0; i < first_bytes / 2; ++i) ((short *)first)[i] = (short)(((int)(i & 127) - 64) * 16);
    if (DS_METHOD(DsUnlock, buffer, 19)(buffer, first, first_bytes, second, second_bytes)) ExitProcess(64);
    if (DS_METHOD(DsPlay, buffer, 12)(buffer, 0, 0, 1)) ExitProcess(65);
    /* This worker never waits: audio must be pumped after resumable CPU slices. */
    if (!CreateThread(NULL, 0, compute_worker, NULL, 0, NULL)) ExitProcess(72);
    for (i = 0; i < 12; ++i) Sleep(20);
    if (DS_METHOD(DsPosition, buffer, 4)(buffer, &position, NULL)) ExitProcess(66);
    if (!position || position >= description.bytes) ExitProcess(67);
    for (i = 0; i < 38; ++i) Sleep(20);
    if (DS_METHOD(DsStop, buffer, 18)(buffer)) ExitProcess(68);
    if (DS_METHOD(DsStatus, buffer, 9)(buffer, &status) || (status & 1)) ExitProcess(69);
    if (DS_METHOD(DsRelease, buffer, 2)(buffer)) ExitProcess(70);
    if (DS_METHOD(DsRelease, device, 2)(device)) ExitProcess(71);
    ExitProcess(0);
}
