/* A setup wizard as installers build one: a property sheet whose pages hold
 * a banner and a long license in a rich edit, a destination with a folder
 * list, and a progress bar, answered page by page through its default
 * button. Then a folder browser with a tree, which closes itself.
 * Build: i686-w64-mingw32-gcc -O2 -Wall wizard.c -o wizard.exe -mwindows -lcomctl32 */
#include <windows.h>
#include <commctrl.h>
#include <prsht.h>
#include <stdio.h>

static int finished;

static INT_PTR CALLBACK page(HWND dialog, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message == WM_INITDIALOG) {
        int kind = (int)((PROPSHEETPAGEW *)lparam)->lParam;
        if (kind == 0) {
            static WCHAR terms[2048];
            for (int i = 0; i < 100; i++) wcscat(terms, L"Terms line\r\n");
            wcscat(terms, L"The end.");
            HWND license = CreateWindowExW(WS_EX_CLIENTEDGE, L"RichEdit20W", terms,
                                           WS_CHILD | WS_VISIBLE | ES_MULTILINE | ES_READONLY | WS_VSCROLL,
                                           7, 30, 300, 100, dialog, (HMENU)100, NULL, NULL);
            printf("license=%s\n", license ? "ok" : "FAIL");
            /* A 4 x 2 banner: blue, green, red, white across, then black. */
            static const DWORD pixels[8] = {0xff0000ff, 0xff00ff00, 0xffff0000, 0xffffffff};
            HBITMAP bitmap = CreateBitmap(4, 2, 1, 32, pixels);
            HWND banner = CreateWindowExW(0, L"Static", L"", WS_CHILD | WS_VISIBLE | SS_BITMAP, 7, 7, 4, 2,
                                          dialog, (HMENU)104, NULL, NULL);
            SendMessageW(banner, STM_SETIMAGE, IMAGE_BITMAP, (LPARAM)bitmap);
        } else if (kind == 1) {
            HWND folders = CreateWindowExW(WS_EX_CLIENTEDGE, WC_LISTVIEWW, L"",
                                           WS_CHILD | WS_VISIBLE | LVS_LIST, 7, 30, 300, 90, dialog,
                                           (HMENU)101, NULL, NULL);
            LVITEMW item = {LVIF_TEXT, 0, 0, 0, 0, L"Program Files"};
            SendMessageW(folders, LVM_INSERTITEMW, 0, (LPARAM)&item);
            CreateWindowExW(WS_EX_CLIENTEDGE, L"Edit", L"C:\\Program Files\\Probe Game",
                            WS_CHILD | WS_VISIBLE, 7, 7, 300, 20, dialog, (HMENU)102, NULL, NULL);
        } else {
            HWND bar = CreateWindowExW(0, PROGRESS_CLASSW, L"", WS_CHILD | WS_VISIBLE, 7, 7, 300, 16,
                                       dialog, (HMENU)103, NULL, NULL);
            SendMessageW(bar, PBM_SETPOS, 100, 0);
        }
        return TRUE;
    }
    if (message == WM_NOTIFY && ((NMHDR *)lparam)->code == PSN_SETACTIVE) {
        HWND sheet = GetParent(dialog);
        int index = PropSheet_HwndToIndex(sheet, dialog);
        PropSheet_SetWizButtons(sheet, index == 2 ? PSWIZB_BACK | PSWIZB_FINISH : PSWIZB_NEXT);
        /* The page is ready for its answer once this is printed. */
        printf("page=%d\n", index);
        fflush(stdout);
        return TRUE;
    }
    if (message == WM_NOTIFY && ((NMHDR *)lparam)->code == PSN_WIZFINISH)
        finished = 1;
    return FALSE;
}

static INT_PTR CALLBACK browser(HWND dialog, UINT message, WPARAM wparam, LPARAM lparam)
{
    if (message == WM_INITDIALOG) {
        HWND tree = CreateWindowExW(WS_EX_CLIENTEDGE, WC_TREEVIEWW, L"", WS_CHILD | WS_VISIBLE | TVS_HASLINES,
                                    7, 7, 200, 100, dialog, (HMENU)200, NULL, NULL);
        TVINSERTSTRUCTW item = {TVI_ROOT, TVI_LAST};
        item.item.mask = TVIF_TEXT;
        item.item.pszText = L"C:";
        SendMessageW(tree, TVM_INSERTITEMW, 0, (LPARAM)&item);
        SetTimer(dialog, 1, 1500, NULL);
        return TRUE;
    }
    if (message == WM_TIMER) {
        EndDialog(dialog, 7);
        return TRUE;
    }
    return FALSE;
}

int main(void)
{
    INITCOMMONCONTROLSEX classes = {sizeof(classes), ICC_WIN95_CLASSES};
    InitCommonControlsEx(&classes);
    LoadLibraryW(L"riched20.dll");
    struct {
        DLGTEMPLATE header;
        WORD menu, class, title;
    } template = {{WS_CHILD | WS_DISABLED | WS_CAPTION, 0, 0, 0, 0, 320, 140}};
    PROPSHEETPAGEW pages[3];
    HPROPSHEETPAGE handles[3];
    for (int i = 0; i < 3; i++) {
        pages[i] = (PROPSHEETPAGEW){sizeof(PROPSHEETPAGEW), PSP_DLGINDIRECT};
        pages[i].pResource = &template.header;
        pages[i].pfnDlgProc = page;
        pages[i].lParam = i;
        handles[i] = CreatePropertySheetPageW(&pages[i]);
    }
    PROPSHEETHEADERW sheet = {sizeof(sheet), PSH_WIZARD};
    sheet.pszCaption = L"Probe Game Setup";
    sheet.nPages = 3;
    sheet.phpage = handles;
    INT_PTR result = PropertySheetW(&sheet);
    printf("result=%d finished=%d\n", (int)result, finished);
    struct {
        DLGTEMPLATE header;
        WORD menu, class, title[7];
    } browse = {{WS_POPUP | WS_CAPTION | WS_SYSMENU | DS_MODALFRAME, 0, 0, 0, 0, 220, 120}, 0, 0,
                {'B', 'r', 'o', 'w', 's', 'e', 0}};
    INT_PTR closed = DialogBoxIndirectW(NULL, &browse.header, NULL, browser);
    printf("browser=%d\n", (int)closed);
    fflush(stdout);
    return finished && result > 0 && closed == 7 ? 0 : 1;
}
