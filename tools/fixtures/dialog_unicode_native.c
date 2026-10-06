/* Authored USER32 Unicode dialog oracle. Templates and strings are synthetic;
   modal callbacks end during initialization, before displaying any window. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

static DWORD report[128], count;
static DWORD template_words[256];
static WORD *cursor;
static HWND owner, dialog;
static const WCHAR title[] = {0x8a2d, 0x5b9a, 0xd83c, 0xdf38, 0};
static const WCHAR initial[] = {0x6f22, 0x5b57, 0xd83d, 0xde00, 0};
static const WCHAR changed[] = {0x96ea, 0xd83d, 0xde00, 0x732b, 0};
static const WCHAR absent[] = {L'M', L'i', L's', L's', L'i', L'n', L'g', L'C', L'l', L'a', L's', L's', 0};
static void emit(DWORD value) { report[count++] = value; }
static void put32(DWORD value) { *cursor++ = (WORD)value; *cursor++ = (WORD)(value >> 16); }
static void text(const WCHAR *value) { do { *cursor++ = *value; } while (*value++); }
static void align4(void) { if ((ULONG_PTR)cursor & 3) *cursor++ = 0; }
static void make_template(BOOL broken) {
    DWORD index;
    volatile DWORD *clear = template_words;
    for (index = 0; index < 256; ++index) clear[index] = 0;
    cursor = (WORD *)template_words;
    put32(WS_POPUP); put32(0);
    *cursor++ = 1; *cursor++ = 1; *cursor++ = 2; *cursor++ = 100; *cursor++ = 60;
    *cursor++ = 0; *cursor++ = 0; text(title);
    align4(); put32(WS_CHILD | WS_VISIBLE | WS_TABSTOP); put32(0);
    *cursor++ = 5; *cursor++ = 5; *cursor++ = 40; *cursor++ = 15; *cursor++ = 41;
    if (broken) text(absent); else { *cursor++ = 0xffff; *cursor++ = 0x81; }
    text(initial); *cursor++ = 0;
}
static INT_PTR CALLBACK callback(HWND hwnd, UINT message, WPARAM wp, LPARAM lp) {
    WCHAR output[16]; DWORD index;
    if (message != WM_INITDIALOG) return FALSE;
    dialog = hwnd;
    if (lp == 9) { DestroyWindow(hwnd); return FALSE; }
    emit((DWORD)lp); emit(IsWindowUnicode(hwnd)); emit(IsWindowUnicode(GetDlgItem(hwnd, 41)));
    emit(wp == (WPARAM)GetDlgItem(hwnd, 41));
    emit(GetDlgItemTextW(hwnd, 41, output, 16));
    for (index = 0; index < 5; ++index) emit(output[index]);
    emit(SetDlgItemTextW(hwnd, 41, changed));
    emit(GetDlgItemTextW(hwnd, 41, output, 3));
    for (index = 0; index < 3; ++index) emit(output[index]);
    emit(SetDlgItemInt(hwnd, 41, 0xfffffff9, TRUE));
    emit(GetDlgItemTextW(hwnd, 41, output, 16)); emit(output[0]); emit(output[1]);
    if (lp == 7) {
        emit(IsWindowEnabled(owner)); emit(EndDialog(hwnd, -7)); emit(IsWindow(hwnd));
    }
    return FALSE;
}
static INT_PTR CALLBACK resource_callback(HWND hwnd, UINT message, WPARAM wp, LPARAM lp) {
    (void)wp;
    if (message == WM_INITDIALOG) { EndDialog(hwnd, lp); return TRUE; }
    return FALSE;
}
void mainCRTStartup(void) {
    DWORD written; HANDLE file; HINSTANCE instance = GetModuleHandleW(NULL);
    make_template(FALSE);
    dialog = CreateDialogIndirectParamW(instance, (DLGTEMPLATE *)template_words, NULL, callback, 0);
    emit(dialog != NULL); emit(IsWindow(dialog)); emit(IsWindowVisible(dialog));
    DestroyWindow(dialog); emit(IsWindow(dialog));
    owner = CreateWindowExA(0, "Static", "owner", WS_POPUP, -30000, -30000, 20, 20, NULL, NULL, instance, NULL);
    emit((DWORD)DialogBoxIndirectParamW(instance, (DLGTEMPLATE *)template_words, owner, callback, 7));
    emit(IsWindowEnabled(owner)); emit(IsWindow(dialog));
    SetLastError(0);
    emit((DWORD)DialogBoxIndirectParamW(instance, (DLGTEMPLATE *)template_words, (HWND)0xbad, callback, 0));
    emit(GetLastError());
    SetLastError(0);
    emit((DWORD)DialogBoxParamW(instance, MAKEINTRESOURCEW(0x99), NULL, callback, 0)); emit(GetLastError());
    make_template(TRUE); SetLastError(0);
    emit((DWORD)DialogBoxIndirectParamW(instance, (DLGTEMPLATE *)template_words, owner, callback, 0));
    emit(GetLastError()); emit(IsWindowEnabled(owner));
    make_template(FALSE);
    emit(CreateDialogIndirectParamW(instance, (DLGTEMPLATE *)template_words, owner, callback, 9) != NULL);
    emit(IsWindow(dialog)); emit(IsWindowEnabled(owner));
    emit((DWORD)DialogBoxIndirectParamW(instance, (DLGTEMPLATE *)template_words, owner, callback, 9));
    emit(IsWindowEnabled(owner));
    DestroyWindow(owner);
#ifdef RESOURCE_DIALOGS
    emit((DWORD)DialogBoxParamW(instance, MAKEINTRESOURCEW(0x66), NULL, resource_callback, 0x12345678));
    emit((DWORD)DialogBoxParamW(instance, L"\x5bfe\x8a71\xd83c\xdf38", NULL, resource_callback, 0x87654321));
#endif
    file = CreateFileW(L"dialog-unicode-native.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, report, count * 4, &written, NULL) || written != count * 4) ExitProcess(2);
    CloseHandle(file); ExitProcess(0);
}
