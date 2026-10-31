/* Authored frame handoff after launcher exit.
 * Build: i686-w64-mingw32-gcc -O2 -Wall display_child.c -o display_child.exe -lgdi32 */
#include <windows.h>
#include <stdio.h>

static LRESULT CALLBACK procedure(HWND window, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message == WM_PAINT) {
        PAINTSTRUCT paint;
        HDC dc = BeginPaint(window, &paint);
        FillRect(dc, &paint.rcPaint, GetStockObject(WHITE_BRUSH));
        EndPaint(window, &paint);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

int main(int argc, char **argv)
{
    if (argc == 1) {
        char exe[MAX_PATH], command[MAX_PATH + 64];
        STARTUPINFOA startup = { sizeof(startup) };
        PROCESS_INFORMATION child;
        GetModuleFileNameA(NULL, exe, sizeof(exe));
        snprintf(command, sizeof(command), "\"%s\" %lu", exe, GetCurrentProcessId());
        if (!CreateProcessA(NULL, command, NULL, NULL, FALSE, 0, NULL, NULL,
                            &startup, &child)) return 1;
        CloseHandle(child.hThread);
        CloseHandle(child.hProcess);
        return 0;
    }
    DWORD pid;
    if (sscanf(argv[1], "%lu", &pid) != 1) return 2;
    HANDLE parent = OpenProcess(SYNCHRONIZE, FALSE, pid);
    if (parent) {
        if (WaitForSingleObject(parent, 10000) != WAIT_OBJECT_0) return 3;
        CloseHandle(parent);
    }
    WNDCLASSW cls = {0};
    cls.lpfnWndProc = procedure;
    cls.hInstance = GetModuleHandleW(NULL);
    cls.lpszClassName = L"display-child";
    if (!RegisterClassW(&cls)) return 4;
    HWND window = CreateWindowExW(0, cls.lpszClassName, L"display child", WS_POPUP | WS_VISIBLE,
                                  0, 0, 80, 40, NULL, NULL, cls.hInstance, NULL);
    if (!window) return 5;
    UpdateWindow(window);
    MSG message;
    while (GetMessageW(&message, NULL, 0, 0) > 0) DispatchMessageW(&message);
    return 0;
}
