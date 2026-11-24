/* An installer as discs ship them. Started from the disc, it stages a copy of
 * itself in %TEMP% and waits for it, as self-extracting setups do, and asks
 * msiexec about a product; the staged copy copies the game into Program
 * Files, unpacks a cabinet beside it, registers it and links it in the Start
 * menu. Started from Program Files, it is that game and finds all of it.
 * Build: i686-w64-mingw32-gcc -O2 -Wall installer.c -o installer.exe -lsetupapi -lole32 -luuid -lshell32 */
#define COBJMACROS
#include <windows.h>
#include <setupapi.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <stdio.h>
#include <wchar.h>

#define VENDOR L"Software\\Probe Vendor\\Probe Game"
#define UNINSTALL L"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\ProbeGame"

/* The staged copy's output may reach no one: its failing step is its exit code. */
static int step;
static void check(const char *name, BOOL ok)
{
    step++;
    printf("%s=%s\n", name, ok ? "ok" : "FAIL");
    fflush(stdout);
    if (!ok) {
        printf("error=%lu\n", GetLastError());
        ExitProcess(100 + step);
    }
}

static void directory_of(WCHAR *path)
{
    WCHAR *slash = wcsrchr(path, L'\\');
    if (slash) slash[slash - path == 2 ? 1 : 0] = 0;
}

static DWORD run(WCHAR *command)
{
    STARTUPINFOW startup = {sizeof(startup)};
    PROCESS_INFORMATION process;
    DWORD code = 0xffffffff;
    if (!CreateProcessW(NULL, command, NULL, NULL, FALSE, 0, NULL, NULL, &startup, &process))
        return code;
    WaitForSingleObject(process.hProcess, INFINITE);
    GetExitCodeProcess(process.hProcess, &code);
    CloseHandle(process.hThread);
    CloseHandle(process.hProcess);
    return code;
}

static void link_path(WCHAR *path)
{
    SHGetFolderPathW(NULL, CSIDL_COMMON_PROGRAMS, NULL, 0, path);
    wcscat(path, L"\\Probe Game.lnk");
}

static UINT CALLBACK unpack(void *target, UINT notification, UINT_PTR param1, UINT_PTR param2)
{
    if (notification == SPFILENOTIFY_FILEINCABINET) {
        FILE_IN_CABINET_INFO_W *file = (FILE_IN_CABINET_INFO_W *)param1;
        swprintf(file->FullTargetName, MAX_PATH, L"%ls\\%ls", (WCHAR *)target, file->NameInCabinet);
        return FILEOP_DOIT;
    }
    return NO_ERROR;
}

static int setup(const WCHAR *self, const WCHAR *root)
{
    WCHAR current[MAX_PATH], temp[MAX_PATH], command[3 * MAX_PATH], system[MAX_PATH];
    GetCurrentDirectoryW(MAX_PATH, current);
    check("starts_in_its_directory", !lstrcmpiW(current, root));
    check("stage", GetTempPathW(MAX_PATH, temp) && wcscat(temp, L"probe-stage.exe") &&
          CopyFileW(self, temp, FALSE));
    swprintf(command, 3 * MAX_PATH, L"\"%ls\" --stage %ls", temp, root);
    DWORD code = run(command);
    printf("stage_exit=%lu\n", code);
    check("staged_copy", code == 0);
    DeleteFileW(temp);
    GetSystemDirectoryW(system, MAX_PATH);
    swprintf(command, 3 * MAX_PATH,
             L"\"%ls\\msiexec.exe\" /x {6B1F3F7A-0D2E-4C1B-9A5E-3C2D1E0F4A5B} /qn", system);
    code = run(command);
    printf("msiexec_exit=%lu\n", code);
    check("msiexec", code == ERROR_UNKNOWN_PRODUCT);
    return 0;
}

static int stage(const WCHAR *root)
{
    WCHAR files[MAX_PATH], target[MAX_PATH], source[MAX_PATH], game[MAX_PATH], lnk[MAX_PATH];
    check("program_files", SHGetFolderPathW(NULL, CSIDL_PROGRAM_FILES, NULL, 0, files) == S_OK &&
          !lstrcmpiW(files + 1, L":\\Program Files"));
    swprintf(target, MAX_PATH, L"%ls\\Probe Vendor\\Probe Game", files);
    check("create_directory", SHCreateDirectoryExW(NULL, target, NULL) == ERROR_SUCCESS);
    swprintf(source, MAX_PATH, L"%lsGAME.EXE", root);
    swprintf(game, MAX_PATH, L"%ls\\game.exe", target);
    check("copy", CopyFileW(source, game, FALSE));
    swprintf(source, MAX_PATH, L"%lsDATA.CAB", root);
    check("cabinet", SetupIterateCabinetW(source, 0, unpack, target));

    HKEY key;
    DWORD version = 3;
    check("register", RegCreateKeyExW(HKEY_LOCAL_MACHINE, VENDOR, 0, NULL, 0, KEY_WRITE, NULL, &key,
                                      NULL) == ERROR_SUCCESS &&
          RegSetValueExW(key, L"InstallDir", 0, REG_SZ, (BYTE *)target,
                         (wcslen(target) + 1) * sizeof(WCHAR)) == ERROR_SUCCESS &&
          RegSetValueExW(key, L"Version", 0, REG_DWORD, (BYTE *)&version, sizeof(version)) ==
              ERROR_SUCCESS);
    RegCloseKey(key);
    check("uninstall_entry",
          RegCreateKeyExW(HKEY_LOCAL_MACHINE, UNINSTALL, 0, NULL, 0, KEY_WRITE, NULL, &key, NULL) ==
                  ERROR_SUCCESS &&
              RegSetValueExW(key, L"DisplayName", 0, REG_SZ, (BYTE *)L"Probe Game",
                             sizeof(L"Probe Game")) == ERROR_SUCCESS);
    RegCloseKey(key);

    IShellLinkW *link;
    IPersistFile *file;
    check("com", SUCCEEDED(CoInitialize(NULL)) &&
          SUCCEEDED(CoCreateInstance(&CLSID_ShellLink, NULL, CLSCTX_INPROC_SERVER, &IID_IShellLinkW,
                                     (void **)&link)));
    link_path(lnk);
    check("shortcut", SUCCEEDED(IShellLinkW_SetPath(link, game)) &&
          SUCCEEDED(IShellLinkW_SetWorkingDirectory(link, target)) &&
          SUCCEEDED(IShellLinkW_QueryInterface(link, &IID_IPersistFile, (void **)&file)) &&
          SUCCEEDED(IPersistFile_Save(file, lnk, TRUE)));
    IPersistFile_Release(file);
    IShellLinkW_Release(link);
    CoUninitialize();
    return 0;
}

static int play(const WCHAR *self, const WCHAR *directory)
{
    WCHAR current[MAX_PATH], value[MAX_PATH], path[MAX_PATH], lnk[MAX_PATH];
    DWORD size = sizeof(value);
    GetCurrentDirectoryW(MAX_PATH, current);
    check("installed_directory", !lstrcmpiW(current, directory));
    check("registered", RegGetValueW(HKEY_LOCAL_MACHINE, VENDOR, L"InstallDir", RRF_RT_REG_SZ, NULL,
                                     value, &size) == ERROR_SUCCESS &&
          !lstrcmpiW(value, directory));
    char text[16] = {0};
    DWORD count;
    HANDLE data = CreateFileW(L"readme.txt", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    check("unpacked", ReadFile(data, text, sizeof(text) - 1, &count, NULL) && !strcmp(text, "from a cabinet"));
    CloseHandle(data);

    IShellLinkW *link;
    IPersistFile *file;
    link_path(lnk);
    check("com", SUCCEEDED(CoInitialize(NULL)) &&
          SUCCEEDED(CoCreateInstance(&CLSID_ShellLink, NULL, CLSCTX_INPROC_SERVER, &IID_IShellLinkW,
                                     (void **)&link)));
    check("shortcut", SUCCEEDED(IShellLinkW_QueryInterface(link, &IID_IPersistFile, (void **)&file)) &&
          SUCCEEDED(IPersistFile_Load(file, lnk, STGM_READ)) &&
          SUCCEEDED(IShellLinkW_GetPath(link, path, MAX_PATH, NULL, 0)) && !lstrcmpiW(path, self));
    IPersistFile_Release(file);
    IShellLinkW_Release(link);
    CoUninitialize();
    printf("played=ok\n");
    return 0;
}

int main(void)
{
    WCHAR self[MAX_PATH], directory[MAX_PATH], root[4];
    int count;
    WCHAR **arguments = CommandLineToArgvW(GetCommandLineW(), &count);
    if (count == 3 && !lstrcmpW(arguments[1], L"--stage"))
        return stage(arguments[2]);
    GetModuleFileNameW(NULL, self, MAX_PATH);
    lstrcpyW(directory, self);
    directory_of(directory);
    lstrcpynW(root, self, 4);
    if (GetDriveTypeW(root) == DRIVE_CDROM)
        return setup(self, directory);
    return play(self, directory);
}
