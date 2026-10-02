/* Minimal x86 Windows ABI declarations; no Windows headers or CRT required. */
#ifndef ADV_FIXTURE_WIN32_MIN_H
#define ADV_FIXTURE_WIN32_MIN_H
typedef unsigned int DWORD;
typedef unsigned int UINT;
typedef unsigned short WORD;
typedef unsigned char BYTE;
typedef int BOOL;
typedef unsigned int HANDLE;
typedef HANDLE HWND;
typedef HANDLE HDC;
typedef HANDLE HBITMAP;
typedef HANDLE HMODULE;
typedef int LONG;
typedef DWORD WPARAM;
typedef LONG LPARAM;
typedef LONG LRESULT;
#define WINAPI __stdcall
#define API __declspec(dllimport)
#define NULL ((void *)0)
#define INFINITE 0xffffffffu
#define WM_CREATE 1
#define WM_DESTROY 2
#define WM_PAINT 15
#define WM_CLOSE 16
#define WM_KEYDOWN 0x100
#define WM_LBUTTONDOWN 0x201
#define SRCCOPY 0x00cc0020u
typedef LRESULT (WINAPI *WNDPROC)(HWND, UINT, WPARAM, LPARAM);
typedef void (WINAPI *TIMERPROC)(HWND, UINT, UINT, DWORD);
typedef void (WINAPI *TLSCALLBACK)(void *, DWORD, void *);
typedef DWORD (WINAPI *THREADPROC)(void *);
typedef struct { LONG x, y; } POINT;
typedef struct { LONG left, top, right, bottom; } RECT;
typedef struct { UINT style; WNDPROC procedure; int class_extra, window_extra; HANDLE instance, icon, cursor, background; const char *menu, *name; } WNDCLASSA;
typedef struct { HWND hwnd; UINT message; WPARAM wparam; LPARAM lparam; DWORD time; POINT point; } MSG;
typedef struct { HDC dc; BOOL erase; RECT paint; BOOL restore, update; BYTE reserved[32]; } PAINTSTRUCT;
typedef struct { DWORD size; LONG width, height; WORD planes, bpp; DWORD compression, image_size; LONG xppm, yppm; DWORD used, important; } BITMAPINFOHEADER;
typedef struct { BITMAPINFOHEADER header; DWORD colors[1]; } BITMAPINFO;
typedef struct { const void *start, *end; DWORD *index; TLSCALLBACK *callbacks; DWORD zero_fill, characteristics; } TLS_DIRECTORY;
API void WINAPI ExitProcess(UINT);
API void WINAPI Sleep(DWORD);
API HANDLE WINAPI CreateEventA(void *, BOOL, BOOL, const char *);
API BOOL WINAPI SetEvent(HANDLE);
API DWORD WINAPI WaitForSingleObject(HANDLE, DWORD);
API HANDLE WINAPI CreateThread(void *, DWORD, THREADPROC, void *, DWORD, DWORD *);
API BOOL WINAPI CloseHandle(HANDLE);
API HMODULE WINAPI GetModuleHandleA(const char *);
API HMODULE WINAPI LoadLibraryA(const char *);
API BOOL WINAPI FreeLibrary(HMODULE);
API void *WINAPI GetProcAddress(HMODULE, const char *);
API WORD WINAPI RegisterClassA(const WNDCLASSA *);
API HWND WINAPI CreateWindowExA(DWORD, const char *, const char *, DWORD, int, int, int, int, HWND, HANDLE, HANDLE, void *);
API BOOL WINAPI ShowWindow(HWND, int);
API BOOL WINAPI UpdateWindow(HWND);
API LRESULT WINAPI DefWindowProcA(HWND, UINT, WPARAM, LPARAM);
API BOOL WINAPI DestroyWindow(HWND);
API void WINAPI PostQuitMessage(int);
API BOOL WINAPI GetMessageA(MSG *, HWND, UINT, UINT);
API BOOL WINAPI TranslateMessage(const MSG *);
API LRESULT WINAPI DispatchMessageA(const MSG *);
API UINT WINAPI SetTimer(HWND, UINT, UINT, TIMERPROC);
API BOOL WINAPI KillTimer(HWND, UINT);
API HDC WINAPI GetDC(HWND);
API int WINAPI ReleaseDC(HWND, HDC);
API HDC WINAPI BeginPaint(HWND, PAINTSTRUCT *);
API BOOL WINAPI EndPaint(HWND, const PAINTSTRUCT *);
API HDC WINAPI CreateCompatibleDC(HDC);
API HBITMAP WINAPI CreateDIBSection(HDC, const BITMAPINFO *, UINT, void **, HANDLE, DWORD);
API HANDLE WINAPI SelectObject(HDC, HANDLE);
API BOOL WINAPI BitBlt(HDC, int, int, int, int, HDC, int, int, DWORD);
API BOOL WINAPI DeleteObject(HANDLE);
API BOOL WINAPI DeleteDC(HDC);
API BOOL WINAPI GdiFlush(void);
#endif
