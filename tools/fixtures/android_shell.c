/* Interactive shell correctness fixture. No game assets, CRT or performance work. */
#include "dsound_min.h"
API HANDLE WINAPI CreateMenu(void);
API BOOL WINAPI AppendMenuA(HANDLE, UINT, UINT, const char *);
API BOOL WINAPI SetMenu(HWND, HANDLE);
API HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
API BOOL WINAPI ReadFile(HANDLE, void *, DWORD, DWORD *, void *);
API short WINAPI GetAsyncKeyState(int);
API LONG WINAPI RegCreateKeyExA(HANDLE, const char *, DWORD, char *, DWORD, DWORD, void *, HANDLE *, DWORD *);
API LONG WINAPI RegSetValueExA(HANDLE, const char *, DWORD, DWORD, const BYTE *, DWORD);
API LONG WINAPI RegQueryValueExA(HANDLE, const char *, DWORD *, DWORD *, BYTE *, DWORD *);
API LONG WINAPI RegCloseKey(HANDLE);

static HWND window;
static HDC memory;
static HBITMAP bitmap;
static DWORD *pixels, value, clicks, downs, ups, saves, loads, registry_value;
static void *sound, *buffer;
static HANDLE key;

static void file_write(const char *path, const void *bytes, DWORD len) {
    DWORD written = 0;
    HANDLE file = CreateFileA(path, 0x40000000u, 0, NULL, 2, 0, 0);
    if (file == 0xffffffffu || !WriteFile(file, bytes, len, &written, NULL) || written != len) ExitProcess(40);
    CloseHandle(file);
}
static void save(void) {
    DWORD bytes[2] = {0x41565331, value};
    file_write("shell-save.bin", bytes, sizeof(bytes));
    if (RegSetValueExA(key, "value", 0, 4, (BYTE *)&value, 4)) ExitProcess(41);
    ++saves;
}
static void load(void) {
    DWORD bytes[2], read = 0, type = 0, len = 4;
    HANDLE file = CreateFileA("shell-save.bin", 0x80000000u, 1, NULL, 3, 0, 0);
    if (file == 0xffffffffu) return;
    if (!ReadFile(file, bytes, sizeof(bytes), &read, NULL) || read != sizeof(bytes) || bytes[0] != 0x41565331) ExitProcess(42);
    CloseHandle(file);
    if (RegQueryValueExA(key, "value", NULL, &type, (BYTE *)&registry_value, &len)
        || type != 4 || len != 4 || registry_value != bytes[1]) ExitProcess(43);
    value = bytes[1]; ++loads;
}
static void draw(void) {
    DWORD x, y, position = 0, status = 0, report[12]; HDC dc;
    if (!pixels) return;
    for (y = 0; y < 240; ++y) for (x = 0; x < 320; ++x)
        pixels[y * 320 + x] = 0xff000000u | ((x * 255 / 319) << 16) | ((y * 255 / 239) << 8) | (value & 255);
    for (y = 16; y < 48; ++y) for (x = 16; x < 48; ++x)
        pixels[y * 320 + x] = (GetAsyncKeyState(17) & 0x8000) ? 0xffffffffu : 0xff203050u;
    for (y = 16; y < 48; ++y) for (x = 64; x < 96; ++x)
        pixels[y * 320 + x] = clicks ? 0xff30ff60u : 0xff203050u;
    dc = GetDC(window);
    if (!BitBlt(dc, 0, 0, 320, 240, memory, 0, 0, SRCCOPY)) ExitProcess(44);
    GdiFlush(); ReleaseDC(window, dc);
    if (buffer) {
        if (DS_METHOD(DsPosition, buffer, 4)(buffer, &position, NULL)) ExitProcess(45);
        if (DS_METHOD(DsStatus, buffer, 9)(buffer, &status)) ExitProcess(46);
    }
    report[0]=0x41565350; report[1]=1; report[2]=value; report[3]=clicks;
    report[4]=downs; report[5]=ups; report[6]=(GetAsyncKeyState(17)&0x8000)!=0;
    report[7]=saves; report[8]=loads; report[9]=registry_value;
    report[10]=position; report[11]=status;
    file_write("probe-state.bin", report, sizeof(report));
}
static LRESULT WINAPI procedure(HWND hwnd, UINT message, WPARAM wp, LPARAM lp) {
    if (message == WM_KEYDOWN) { ++downs; if (wp == 13) ++value; draw(); return 0; }
    if (message == 0x101) { ++ups; draw(); return 0; }
    if (message == WM_LBUTTONDOWN) { ++clicks; draw(); return 0; }
    if (message == 0x111) { if ((wp & 0xffff)==5001) save(); if ((wp & 0xffff)==5002) load(); draw(); return 0; }
    if (message == 0x113) { draw(); return 0; }
    if (message == WM_PAINT) { PAINTSTRUCT ps; BeginPaint(hwnd,&ps); EndPaint(hwnd,&ps); draw(); return 0; }
    if (message == WM_CLOSE) { DestroyWindow(hwnd); return 0; }
    if (message == WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcA(hwnd,message,wp,lp);
}
void mainCRTStartup(void) {
    WNDCLASSA wc = {0}; BITMAPINFO bi = {0}; MSG msg; HANDLE menu;
    WAVEFORMATEX format={1,1,44100,88200,2,16,0}; DSBUFFERDESC desc={0};
    void *first=NULL,*second=NULL; DWORD n=0,m=0,i,disposition;
    wc.procedure=procedure; wc.instance=GetModuleHandleA(NULL); wc.name="AdvAndroidShellProbe";
    if (!RegisterClassA(&wc)) ExitProcess(50);
    window=CreateWindowExA(0,wc.name,"ARM shell correctness",0x10cf0000u,0,0,336,279,0,0,wc.instance,NULL);
    if (!window) ExitProcess(51);
    memory=CreateCompatibleDC(0); bi.header.size=sizeof(BITMAPINFOHEADER);
    bi.header.width=320; bi.header.height=-240; bi.header.planes=1; bi.header.bpp=32;
    bitmap=CreateDIBSection(memory,&bi,0,(void **)&pixels,0,0);
    if (!memory || !bitmap || !pixels) ExitProcess(52);
    SelectObject(memory,bitmap);
    menu=CreateMenu(); AppendMenuA(menu,0,5001,"Save"); AppendMenuA(menu,0,5002,"Load"); SetMenu(window,menu);
    if (RegCreateKeyExA(0x80000001u,"Software\\AdvShellProbe",0,NULL,0,0xf003f,NULL,&key,&disposition)) ExitProcess(53);
    load();
    desc.size=sizeof(desc); desc.flags=8; desc.bytes=88200; desc.format=&format;
    if (DirectSoundCreate(NULL,&sound,NULL)) ExitProcess(54);
    if (DS_METHOD(DsCreateBuffer,sound,3)(sound,&desc,&buffer,NULL)) ExitProcess(55);
    if (DS_METHOD(DsLock,buffer,11)(buffer,0,desc.bytes,&first,&n,&second,&m,0) || n!=desc.bytes || m) ExitProcess(56);
    for(i=0;i<n/2;++i) ((short *)first)[i]=(short)(((int)(i&127)-64)*8);
    if (DS_METHOD(DsUnlock,buffer,19)(buffer,first,n,second,m)) ExitProcess(57);
    if (DS_METHOD(DsPlay,buffer,12)(buffer,0,0,1)) ExitProcess(58);
    ShowWindow(window,5); UpdateWindow(window); SetTimer(window,1,100,NULL); draw();
    while(GetMessageA(&msg,0,0,0)) { TranslateMessage(&msg); DispatchMessageA(&msg); }
    DS_METHOD(DsStop,buffer,18)(buffer); DS_METHOD(DsRelease,buffer,2)(buffer); DS_METHOD(DsRelease,sound,2)(sound);
    RegCloseKey(key); DeleteObject(bitmap); DeleteDC(memory); ExitProcess(0);
}
