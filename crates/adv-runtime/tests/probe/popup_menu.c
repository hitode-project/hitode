/* A modal popup with a nested command, then a second popup to cancel.
 * Build: i686-w64-mingw32-gcc -O1 -o popup_menu.exe popup_menu.c -luser32 -lgdi32 */
#include <windows.h>
#include <stdio.h>

static unsigned initialized, commands, syscommands;
static const unsigned char memory[] = {0x19, 0x28, 0x37, 0x46};

static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wp, LPARAM lp)
{
    switch (message) {
    case WM_PAINT: {
        PAINTSTRUCT paint;
        HDC dc = BeginPaint(window, &paint);
        RECT rect;
        GetClientRect(window, &rect);
        FillRect(dc, &rect, GetStockObject(WHITE_BRUSH));
        EndPaint(window, &paint);
        return 0;
    }
    case WM_INITMENUPOPUP:
        ++initialized;
        printf("init-popup=%u\n", initialized);
        break;
    case WM_MENUSELECT:
        printf("select=%u flags=%u\n", LOWORD(wp), HIWORD(wp));
        break;
    case WM_COMMAND:
        ++commands;
        break;
    case WM_SYSCOMMAND:
        ++syscommands;
        break;
    case WM_APP: {
        HMENU menu = CreatePopupMenu(), sub = CreatePopupMenu();
        POINT point = {12, 16};
        UINT picked, cancelled;
        AppendMenuW(sub, MF_STRING, 42, L"Nested choice");
        AppendMenuW(menu, MF_POPUP, (UINT_PTR)sub, L"More");
        AppendMenuW(menu, MF_STRING, 43, L"Other choice");
        ClientToScreen(window, &point);
        picked = TrackPopupMenuEx(menu, TPM_RETURNCMD, point.x, point.y, window, NULL);
        printf("picked=%u initialized=%u commands=%u syscommands=%u\n",
               picked, initialized, commands, syscommands);
        initialized = 0;
        cancelled = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_NONOTIFY,
                                     point.x, point.y, window, NULL);
        printf("cancelled=%u initialized=%u commands=%u syscommands=%u\n",
               cancelled, initialized, commands, syscommands);
        DestroyMenu(menu);
        DestroyWindow(window);
        PostQuitMessage(picked == 42 && cancelled == 0 && !initialized && !commands && !syscommands ? 0 : 1);
        return 0;
    }
    }
    return DefWindowProcW(window, message, wp, lp);
}

int main(void)
{
    WNDCLASSW class = {0};
    HWND window;
    MSG message;
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("memory=%p\n", memory);
    class.lpfnWndProc = procedure;
    class.hInstance = GetModuleHandleW(NULL);
    class.lpszClassName = L"PopupProbe";
    RegisterClassW(&class);
    window = CreateWindowW(class.lpszClassName, L"Popup probe", WS_OVERLAPPEDWINDOW,
                           80, 60, 240, 160, NULL, NULL, class.hInstance, NULL);
    ShowWindow(window, SW_SHOW);
    UpdateWindow(window);
    PostMessageW(window, WM_APP, 0, 0);
    while (GetMessageW(&message, NULL, 0, 0) > 0) {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    return message.wParam;
}
