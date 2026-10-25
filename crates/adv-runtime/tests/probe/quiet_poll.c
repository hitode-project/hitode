/* Headless Wine polling and notification probe.
 * Build: i686-w64-mingw32-gcc -O1 -o quiet_poll.exe quiet_poll.c -lgdi32 -lwinmm */
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>

static HWND window;
static HANDLE events[3], read_event;
static volatile DWORD polls;
static unsigned seen;

static LRESULT CALLBACK procedure(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam)
{
    unsigned source = 0;
    if (message == WM_PAINT)
    {
        PAINTSTRUCT paint;
        HDC dc = BeginPaint(hwnd, &paint);
        FillRect(dc, &paint.rcPaint, GetStockObject(BLACK_BRUSH));
        EndPaint(hwnd, &paint);
        return 0;
    }
    if (message == WM_KEYDOWN) source = 1;
    if (message == WM_APP) source = (unsigned)wparam;
    if (message == WM_APP + 1) SendMessageW(hwnd, WM_APP, 6, 0);
    if (source && source <= 6)
    {
        seen |= 1u << (source - 1);
        printf("observed=%u\n", source);
        fflush(stdout);
        return 0;
    }
    return DefWindowProcW(hwnd, message, wparam, lparam);
}

static DWORD WINAPI notifications(void *unused)
{
    for (unsigned i = 0; i < 2; ++i)
    {
        WaitForSingleObject(events[i], INFINITE);
        PostMessageW(window, WM_APP, i + 3, 0);
    }
    WaitForSingleObject(read_event, INFINITE);
    PostMessageW(window, WM_APP, 5, 0);
    return 0;
}

static DWORD WINAPI write_pipe(void *handle)
{
    DWORD written;
    WaitForSingleObject(events[2], INFINITE);
    return !WriteFile(handle, "x", 1, &written, NULL);
}

static void peek(void)
{
    MSG message;
    ++polls;
    while (PeekMessageW(&message, NULL, 0, 0, PM_REMOVE)) DispatchMessageW(&message);
}

int main(void)
{
    WNDCLASSW cls = {0};
    OVERLAPPED overlapped = {0};
    DWORD bytes, start;
    char data;
    cls.lpfnWndProc = procedure;
    cls.hInstance = GetModuleHandleW(NULL);
    cls.lpszClassName = L"quiet";
    if (!RegisterClassW(&cls)) return 2;
    window = CreateWindowExW(0, cls.lpszClassName, L"quiet", WS_POPUP | WS_VISIBLE,
                             0, 0, 128, 64, NULL, NULL, cls.hInstance, NULL);
    if (!window) return 3;
    UpdateWindow(window);
    for (unsigned i = 0; i < 3; ++i) events[i] = CreateEventW(NULL, FALSE, FALSE, NULL);
    read_event = overlapped.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    HANDLE pipe = CreateNamedPipeW(L"\\\\.\\pipe\\quiet-poll", PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED,
                                   PIPE_TYPE_BYTE | PIPE_WAIT, 1, 1024, 1024, 0, NULL);
    if (pipe == INVALID_HANDLE_VALUE) return 4;
    HANDLE writer = CreateFileW(L"\\\\.\\pipe\\quiet-poll", GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
    if (writer == INVALID_HANDLE_VALUE) return 5;
    if (ReadFile(pipe, &data, 1, &bytes, &overlapped) || GetLastError() != ERROR_IO_PENDING) return 6;
    HANDLE worker = CreateThread(NULL, 0, notifications, NULL, 0, NULL);
    HANDLE io = CreateThread(NULL, 0, write_pipe, writer, 0, NULL);
    if (!worker || !io) return 7;
    printf("ready=%lx event=%lx audio=%lx io=%lx polls=%lx\n", (DWORD)window,
           (DWORD)events[0], (DWORD)events[1], (DWORD)events[2], (DWORD)&polls);
    fflush(stdout);
    start = timeGetTime();
    while (timeGetTime() - start < 100) peek();
    printf("deadline=%lu\n", timeGetTime() - start);
    fflush(stdout);
    start = timeGetTime();
    while (seen != 63 && timeGetTime() - start < 10000) peek();
    if (seen != 63) return 8;
    if (WaitForSingleObject(worker, 1000) || WaitForSingleObject(io, 1000)) return 9;
    DestroyWindow(window);
    return data != 'x';
}
