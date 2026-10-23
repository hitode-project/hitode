/* The window procedure probe: user32 calls back into a window procedure
 * from CreateWindow, SendMessage and DispatchMessage. Inside the callback
 * the procedure draws into the window, sends itself a nested message and
 * waits; every call must return its value to the right caller.
 * Build: i686-w64-mingw32-gcc -O1 -o wndproc.exe wndproc.c -lgdi32 */
#include <windows.h>
#include <stdio.h>

#define WM_DRAW (WM_USER + 1)
#define WM_NESTED (WM_USER + 2)
#define WM_POSTED (WM_USER + 3)

static int created, nested, drawn, posted, destroyed, depth, deepest;
static DWORD waited;
static HANDLE event;

static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
{
    LRESULT result;

    switch (message)
    {
    case WM_CREATE:
        created = 1;
        return 0;
    case WM_DRAW:
    {
        HDC dc = GetDC(window);
        RECT rect = {0, 0, 8, 8};
        if (++depth > deepest) deepest = depth;
        drawn = FillRect(dc, &rect, GetStockObject(BLACK_BRUSH)) &&
                TextOutW(dc, 0, 0, L"x", 1);
        ReleaseDC(window, dc);
        nested = SendMessageW(window, WM_NESTED, 3, 0);
        waited = WaitForSingleObject(event, 10);
        depth--;
        return 40 + nested;
    }
    case WM_NESTED:
        if (++depth > deepest) deepest = depth;
        result = wparam * 2 + 1;
        depth--;
        return result;
    case WM_POSTED:
        posted = SendMessageW(window, WM_NESTED, wparam, 0);
        PostQuitMessage(5);
        return 0;
    case WM_DESTROY:
        destroyed = 1;
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

int main(void)
{
    WNDCLASSW class = {0};
    HWND window;
    LRESULT sent;
    MSG message;
    int quit = -1;

    event = CreateEventW(NULL, TRUE, FALSE, NULL);
    class.lpfnWndProc = procedure;
    class.hInstance = GetModuleHandleW(NULL);
    class.lpszClassName = L"probe";
    if (!RegisterClassW(&class)) return 2;
    window = CreateWindowExW(0, L"probe", L"probe", WS_OVERLAPPEDWINDOW, 0, 0, 64, 32,
                             NULL, NULL, class.hInstance, NULL);
    if (!window) return 3;

    sent = SendMessageW(window, WM_DRAW, 0, 0);
    PostMessageW(window, WM_POSTED, 10, 0);
    while (GetMessageW(&message, NULL, 0, 0) > 0) DispatchMessageW(&message);
    if (message.message == WM_QUIT) quit = message.wParam;
    DestroyWindow(window);

    printf("created=%d sent=%ld nested=%d deepest=%d drawn=%d waited=%lu posted=%d quit=%d destroyed=%d\n",
           created, (long)sent, nested, deepest, drawn, waited, posted, quit, destroyed);
    fflush(stdout);
    return !(created && sent == 47 && nested == 7 && deepest == 2 && drawn &&
             waited == WAIT_TIMEOUT && posted == 21 && quit == 5 && destroyed);
}
