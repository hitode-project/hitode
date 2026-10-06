/* Owned USER32 WINDOWPLACEMENT probe, compiled with the Windows SDK ABI. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

void *memset(void *p, int v, unsigned n) { BYTE *b = p; while (n--) *b++ = (BYTE)v; return p; }
static DWORD words[1024], used;
static void record(DWORD value) { if (used == 1024) ExitProcess(90); words[used++] = value; }
static void snapshot(HWND hwnd) {
    WINDOWPLACEMENT placement;
    memset(&placement, 0xa5, sizeof(placement));
    placement.length = sizeof(placement);
    SetLastError(0x1234);
    record(GetWindowPlacement(hwnd, &placement));
    record(GetLastError());
    for (UINT i = 0; i < sizeof(placement) / 4; ++i) record(((DWORD *)&placement)[i]);
}
static void save(void) {
    DWORD n;
    HANDLE file = CreateFileA("window-placement.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, words, used * 4, &n, NULL) || n != used * 4) ExitProcess(91);
    CloseHandle(file);
}
void mainCRTStartup(void) {
    /* Every shown window is owned, nonactivating and omitted from the taskbar. */
    HWND window = CreateWindowExA(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, "Static", "owned-placement-probe", WS_OVERLAPPEDWINDOW, 110, 120, 320, 240, NULL, NULL, NULL, NULL);
    if (!window) ExitProcess(92);
    snapshot(window);
    MoveWindow(window, 150, 170, 360, 260, FALSE); snapshot(window);
    ShowWindow(window, SW_SHOWNA); snapshot(window);
    ShowWindow(window, SW_SHOWMAXIMIZED); snapshot(window);
    ShowWindow(window, SW_MINIMIZE); snapshot(window);
    ShowWindow(window, SW_RESTORE); snapshot(window);
    ShowWindow(window, SW_SHOWNORMAL); snapshot(window);
    ShowWindow(window, SW_SHOWMINNOACTIVE); snapshot(window);
    ShowWindow(window, SW_SHOWNOACTIVATE); snapshot(window);
    ShowWindow(window, SW_HIDE); snapshot(window);
    HWND child = CreateWindowExA(0, "Static", NULL, WS_CHILD, 20, 30, 90, 80, window, NULL, NULL, NULL);
    HWND tool = CreateWindowExA(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, "Static", NULL, WS_POPUP, 210, 220, 90, 80, NULL, NULL, NULL, NULL);
    snapshot(child); snapshot(tool);
    ShowWindow(window, SW_SHOWMAXIMIZED); ShowWindow(window, SW_MINIMIZE);
    ShowWindow(window, SW_SHOWNOACTIVATE); snapshot(window); ShowWindow(window, SW_HIDE);
    ShowWindow(child, SW_SHOWMINNOACTIVE); snapshot(child);
    ShowWindow(tool, SW_SHOWMINNOACTIVE); snapshot(tool); ShowWindow(tool, SW_HIDE);
    HWND ordinary = CreateWindowExA(WS_EX_NOACTIVATE, "Static", NULL, WS_OVERLAPPEDWINDOW, 310, 320, 410, 290, NULL, NULL, NULL, NULL);
    snapshot(ordinary); /* Hidden ordinary top-level workspace coordinates. */
    ShowWindow(window, SW_SHOWNORMAL); ShowWindow(window, SW_SHOWMINNOACTIVE);
    MoveWindow(window, 350, 370, 460, 360, FALSE); snapshot(window);
    ShowWindow(window, SW_RESTORE); snapshot(window); ShowWindow(window, SW_HIDE);
    ShowWindow(window, SW_SHOWMAXIMIZED);
    MoveWindow(window, 450, 470, 560, 460, FALSE); snapshot(window);
    ShowWindow(window, SW_MINIMIZE); ShowWindow(window, SW_RESTORE); snapshot(window); ShowWindow(window, SW_HIDE);
    UINT lengths[] = { 0, 1, 43, 44, 45, 60 };
    WINDOWPLACEMENT placement;
    for (UINT i = 0; i != sizeof(lengths) / sizeof(lengths[0]); ++i) {
        memset(&placement, 0xa5, sizeof(placement)); placement.length = lengths[i];
        SetLastError(0x1234); record(GetWindowPlacement(window, &placement)); record(GetLastError());
        record(placement.length); record(placement.flags);
    }
    SetLastError(0x1234); record(GetWindowPlacement(window, NULL)); record(GetLastError());
    placement.length = sizeof(placement); SetLastError(0x1234); record(GetWindowPlacement((HWND)0x1234, &placement)); record(GetLastError());
    placement.length = 0; SetLastError(0x1234); record(GetWindowPlacement((HWND)0x1234, &placement)); record(GetLastError());
    BYTE *protected = VirtualAlloc(NULL, 8192, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    DWORD old;
    if (!protected || !VirtualProtect(protected + 4096, 4096, PAGE_READONLY, &old)) ExitProcess(93);
    memset(protected, 0xa5, 4096);
    SetLastError(0x1234); record(GetWindowPlacement(window, (WINDOWPLACEMENT *)(protected + 4096))); record(GetLastError());
    SetLastError(0x1234); record(GetWindowPlacement(window, (WINDOWPLACEMENT *)(protected + 4096 - 24))); record(GetLastError());
    record(*(DWORD *)(protected + 4096 - 24)); record(*(DWORD *)(protected + 4096 - 4));
    DestroyWindow(ordinary);
    DestroyWindow(child); DestroyWindow(tool); DestroyWindow(window);
    placement.length = sizeof(placement); SetLastError(0x1234); record(GetWindowPlacement(window, &placement)); record(GetLastError());
    record(0x504c0001); save(); ExitProcess(0);
}
