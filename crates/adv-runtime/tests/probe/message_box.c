/* A message box over a painted window, answered by the host.
 * Build: i686-w64-mingw32-gcc -O1 -o message_box.exe message_box.c -luser32 -lgdi32 */
#include <windows.h>
#include <stdio.h>

static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wp, LPARAM lp)
{
    if (message == WM_PAINT) {
        PAINTSTRUCT paint;
        HDC dc = BeginPaint(window, &paint);
        RECT rect;
        GetClientRect(window, &rect);
        FillRect(dc, &rect, GetStockObject(WHITE_BRUSH));
        EndPaint(window, &paint);
        return 0;
    }
    return DefWindowProcW(window, message, wp, lp);
}

int main(void)
{
    WNDCLASSW class = {0};
    class.lpfnWndProc = procedure;
    class.hInstance = GetModuleHandleW(NULL);
    class.lpszClassName = L"message_box_probe";
    RegisterClassW(&class);
    HWND window = CreateWindowW(L"message_box_probe", L"probe", WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                                0, 0, 320, 240, NULL, NULL, class.hInstance, NULL);
    UpdateWindow(window);
    int answer = MessageBoxW(window, L"Quit the game?", L"probe", MB_OKCANCEL | MB_ICONQUESTION);
    printf("answer=%d\n", answer);
    fflush(stdout);
    DestroyWindow(window);
    return 0;
}
