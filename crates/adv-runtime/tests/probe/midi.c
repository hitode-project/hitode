/* Authored WinMM MIDI, stream and MCI sequencer checks.
 * Build: i686-w64-mingw32-gcc -O2 -Wall midi.c -o midi.exe -lwinmm */
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>

static unsigned opens, closes, done, positions;
static HANDLE completed;
static void check(const char *name, BOOL ok)
{
    printf("%s=%s\n", name, ok ? "ok" : "FAIL");
    fflush(stdout);
    if (!ok) ExitProcess(1);
}
static void CALLBACK callback(HMIDIOUT midi, UINT message, DWORD_PTR instance,
                             DWORD_PTR param1, DWORD_PTR param2)
{
    (void)midi; (void)instance; (void)param1; (void)param2;
    if (message == MOM_OPEN) ++opens;
    if (message == MOM_CLOSE) ++closes;
    if (message == MOM_DONE) { ++done; SetEvent(completed); }
    if (message == MOM_POSITIONCB) ++positions;
}
static void short_message(HMIDIOUT midi, BYTE status, BYTE first, BYTE second)
{
    check("short", midiOutShortMsg(midi, status | (first << 8) | (second << 16)) == 0);
}
static const unsigned char song[] = {
    'M','T','h','d',0,0,0,6, 0,0,0,1,1,0xe0,
    'M','T','r','k',0,0,0,23,
    0,0xff,0x51,3,7,0xa1,0x20,
    0,0xc0,0,
    0,0x90,60,100,
    0x83,0x60,0x80,60,0,
    0,0xff,0x2f,0
};
int main(void)
{
    HMIDIOUT midi, rejected = NULL;
    HMIDISTRM stream;
    MIDIHDR header = {0};
    MIDIOUTCAPSW caps;
    DWORD events[23] = {0};
    MIDIPROPTIMEDIV division = {sizeof(division), 480};
    MIDIPROPTEMPO tempo = {sizeof(tempo), 500000};
    MMTIME before = {TIME_TICKS}, after = {TIME_TICKS};
    BYTE reset[] = {0xf0,0x7e,0x7f,0x09,0x01,0xf7};
    UINT device = MIDI_MAPPER;
    DWORD volume, written;
    HANDLE file;
    char result[80];
    completed = CreateEventA(NULL, FALSE, FALSE, NULL);
    check("output-device", midiOutGetNumDevs() == 1);
    check("no-input", midiInGetNumDevs() == 0);
    check("caps", midiOutGetDevCapsW(0, &caps, sizeof(caps)) == 0 &&
          caps.wTechnology == MOD_SWSYNTH && caps.wChannelMask == 0xffff);
    check("bad-device", midiOutOpen(&rejected, 1, (DWORD_PTR)callback, 0, CALLBACK_FUNCTION) == MMSYSERR_BADDEVICEID && !rejected && !opens);
    check("open", midiOutOpen(&midi, 0, (DWORD_PTR)callback, 0, CALLBACK_FUNCTION) == 0);
    check("open-callback", opens == 1);
    check("allocated", midiOutOpen(&rejected, 0, (DWORD_PTR)callback, 0, CALLBACK_FUNCTION) == MMSYSERR_ALLOCATED && !rejected && opens == 1);
    check("volume-set", midiOutSetVolume(midi, 0x7fff3fff) == 0);
    check("volume-get", midiOutGetVolume(midi, &volume) == 0 && volume == 0x7fff3fff);
    short_message(midi, 0xc0, 0, 0);
    short_message(midi, 0x90, 60, 100);
    Sleep(120);
    short_message(midi, 0xb0, 64, 127);
    short_message(midi, 0x80, 60, 0);
    Sleep(60);
    short_message(midi, 0xb0, 64, 0);
    short_message(midi, 0xc1, 40, 0);
    short_message(midi, 0x91, 67, 100);
    short_message(midi, 0xe1, 0, 80);
    short_message(midi, 0x99, 38, 100);
    Sleep(120);
    header.lpData = (char *)reset;
    header.dwBufferLength = sizeof(reset);
    check("unprepared", midiOutLongMsg(midi, &header, sizeof(header)) == MIDIERR_UNPREPARED && !done);
    check("prepare", midiOutPrepareHeader(midi, &header, sizeof(header)) == 0 && (header.dwFlags & MHDR_PREPARED));
    check("sysex", midiOutLongMsg(midi, &header, sizeof(header)) == 0);
    check("done", WaitForSingleObject(completed, 2000) == WAIT_OBJECT_0 && done == 1 &&
          (header.dwFlags & MHDR_DONE) && !(header.dwFlags & MHDR_INQUEUE));
    check("unprepare", midiOutUnprepareHeader(midi, &header, sizeof(header)) == 0 && !(header.dwFlags & MHDR_PREPARED));
    check("reset", midiOutReset(midi) == 0);
    check("close", midiOutClose(midi) == 0 && closes == 1);

    check("stream-open", midiStreamOpen(&stream, &device, 1, (DWORD_PTR)callback, 0, CALLBACK_FUNCTION) == 0);
    check("stream-division", midiStreamProperty(stream, (BYTE *)&division, MIDIPROP_SET | MIDIPROP_TIMEDIV) == 0);
    check("stream-tempo", midiStreamProperty(stream, (BYTE *)&tempo, MIDIPROP_SET | MIDIPROP_TEMPO) == 0);
    events[2] = 0x00643c90 | MEVT_F_CALLBACK;
    events[3] = 240;
    events[5] = (MEVT_TEMPO << 24) | 250000;
    events[6] = 240;
    events[8] = 0x00003c80;
    events[11] = 0x00644090;
    events[12] = 240;
    events[14] = (MEVT_NOP << 24) | MEVT_F_CALLBACK;
    events[17] = 0x00004080;
    events[20] = (MEVT_LONGMSG << 24) | sizeof(reset);
    memcpy(&events[21], reset, sizeof(reset));
    ZeroMemory(&header, sizeof(header));
    header.lpData = (char *)events;
    header.dwBufferLength = sizeof(events);
    header.dwBytesRecorded = sizeof(events);
    check("stream-prepare", midiOutPrepareHeader((HMIDIOUT)stream, &header, sizeof(header)) == 0);
    check("stream-out", midiStreamOut(stream, &header, sizeof(header)) == 0);
    check("stream-start", midiStreamRestart(stream) == 0);
    Sleep(80);
    check("stream-pause", midiStreamPause(stream) == 0);
    check("stream-position", midiStreamPosition(stream, &before, sizeof(before)) == 0);
    Sleep(60);
    check("stream-paused-position", midiStreamPosition(stream, &after, sizeof(after)) == 0 && before.u.ticks == after.u.ticks);
    check("stream-resume", midiStreamRestart(stream) == 0);
    check("stream-done", WaitForSingleObject(completed, 3000) == WAIT_OBJECT_0 && done == 2 && positions == 2);
    check("stream-stop", midiStreamStop(stream) == 0);
    check("stream-unprepare", midiOutUnprepareHeader((HMIDIOUT)stream, &header, sizeof(header)) == 0);
    check("stream-close", midiStreamClose(stream) == 0);

    file = CreateFileA("midi-probe.mid", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    check("mci-file", file != INVALID_HANDLE_VALUE && WriteFile(file, song, sizeof(song), &written, NULL) && written == sizeof(song));
    CloseHandle(file);
    check("mci-open", mciSendStringA("open midi-probe.mid type sequencer alias score", NULL, 0, NULL) == 0);
    check("mci-play", mciSendStringA("play score wait", NULL, 0, NULL) == 0);
    check("mci-mode", mciSendStringA("status score mode", result, sizeof(result), NULL) == 0 && !lstrcmpA(result, "stopped"));
    check("mci-seek", mciSendStringA("seek score to start", NULL, 0, NULL) == 0);
    check("mci-close", mciSendStringA("close score", NULL, 0, NULL) == 0);
    DeleteFileA("midi-probe.mid");
    CloseHandle(completed);
    puts("midi ok");
    return 0;
}
