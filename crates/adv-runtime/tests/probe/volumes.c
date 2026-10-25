/* Authored ISO data, mappings, inherited owners and installation paths.
 * Build: i686-w64-mingw32-gcc -O2 -Wall volumes.c -o volumes.exe */
#include <windows.h>
#include <winioctl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define DISC(path) L"D:\\" path /* privacy: allow authored guest optical paths */
#define DISC2(path) L"G:\\" path /* privacy: allow authored guest optical paths */

static void check(const char *name, BOOL ok)
{
    printf("%s=%s\n", name, ok ? "ok" : "FAIL"); fflush(stdout);
    if (!ok) { printf("error=%lu\n", GetLastError()); ExitProcess(1); }
}
static HANDLE open_read(const WCHAR *name)
{
    HANDLE file = CreateFileW(name, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    check("open", file != INVALID_HANDLE_VALUE);
    return file;
}
int main(void)
{
    char bytes[32] = {0}; DWORD count;
    if (strstr(GetCommandLineA(), "--child")) {
        HANDLE file, mapping;
        char *arguments = strstr(GetCommandLineA(), "--child") + 7;
        unsigned long a, b;
        check("child_args", sscanf(arguments, "%lu %lu", &a, &b) == 2);
        file = (HANDLE)(ULONG_PTR)a; mapping = (HANDLE)(ULONG_PTR)b;
        Sleep(100);
        check("inherited_read", ReadFile(file, bytes, 11, &count, NULL) && count == 11 && !memcmp(bytes, "hello world", 11));
        const char *view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
        check("inherited_map", view && !memcmp(view, "hello world", 11));
        UnmapViewOfFile(view); CloseHandle(mapping); CloseHandle(file);
        file = open_read(L"C:\\Program Files\\Volume Probe\\renamed.bin");
        check("inherited_install", ReadFile(file, bytes, 9, &count, NULL) && count == 9 && !memcmp(bytes, "installed", 9));
        CloseHandle(file);
        return 0;
    }
    if (GetFileAttributesA("reset.signal") != INVALID_FILE_ATTRIBUTES) {
        WCHAR device[MAX_PATH];
        check("reset_namespace", !(GetLogicalDrives() & (1 << 6)) &&
              QueryDosDeviceW(L"D:", device, MAX_PATH) &&
              !lstrcmpW(device, L"\\Device\\HarddiskVolume2"));
        check("reset", GetDriveTypeW(DISC(L"")) == DRIVE_FIXED && GetDriveTypeW(DISC2(L"")) == DRIVE_NO_ROOT_DIR &&
              GetFileAttributesW(L"C:\\Program Files\\Volume Probe\\renamed.bin") == INVALID_FILE_ATTRIBUTES);
        return 0;
    }
    WCHAR path[MAX_PATH], label[64], filesystem[32]; DWORD serial, max_name, flags;
    GetCurrentDirectoryW(MAX_PATH, path);
    check("game_drive", path[0] == 'E' && path[1] == ':');
    check("drive_types", GetDriveTypeW(DISC(L"")) == DRIVE_CDROM && GetDriveTypeW(DISC2(L"")) == DRIVE_CDROM);
    check("drives", (GetLogicalDrives() & ((1 << 3) | (1 << 4) | (1 << 6))) == ((1 << 3) | (1 << 4) | (1 << 6)));
    check("volume", GetVolumeInformationW(DISC(L""), label, 64, &serial, &max_name, &flags, filesystem, 32) &&
          !lstrcmpW(label, L"OPTICAL_TEST") && !lstrcmpW(filesystem, L"CDFS") && (flags & FILE_READ_ONLY_VOLUME));
    check("device_name", QueryDosDeviceW(L"D:", path, MAX_PATH) && !lstrcmpW(path, L"\\Device\\CdRom0"));
    ULARGE_INTEGER available, total, free_bytes;
    check("capacity", GetDiskFreeSpaceExW(DISC(L""), &available, &total, &free_bytes) && !available.QuadPart && !free_bytes.QuadPart);
    printf("serial=%lu capacity=%llu max_name=%lu\n", serial, total.QuadPart, max_name); fflush(stdout);
    WIN32_FIND_DATAW found;
    HANDLE search = FindFirstFileW(DISC(L"Sub\\*"), &found);
    check("enumerate", search != INVALID_HANDLE_VALUE);
    int unicode = 0, split = 0;
    do { unicode |= !lstrcmpW(found.cFileName, L"\x65e5\x672c.txt"); split |= !lstrcmpW(found.cFileName, L"split.bin"); } while (FindNextFileW(search, &found));
    FindClose(search);
    check("joliet", unicode && split);
    check("short_name", GetShortPathNameW(DISC(L"Sub\\\x65e5\x672c.txt"), path, MAX_PATH) > 0);
    HANDLE short_file = open_read(path);
    check("short_read", ReadFile(short_file, bytes, sizeof(bytes), &count, NULL) && count == 5 && !memcmp(bytes, "abcde", 5));
    CloseHandle(short_file);
    HANDLE file = open_read(DISC(L"readme.TXT")), second = open_read(DISC2(L"README.txt"));
    check("seek", SetFilePointer(file, 6, NULL, FILE_BEGIN) == 6);
    check("read", ReadFile(file, bytes, sizeof(bytes), &count, NULL) && count == 5 && !memcmp(bytes, "world", 5));
    check("independent_cursor", ReadFile(second, bytes, 5, &count, NULL) && count == 5 && !memcmp(bytes, "hello", 5));
    CloseHandle(second);
    check("eof", ReadFile(file, bytes, sizeof(bytes), &count, NULL) && count == 0);
    BY_HANDLE_FILE_INFORMATION info;
    check("attributes", GetFileInformationByHandle(file, &info) && info.nFileSizeLow == 11 && (info.dwFileAttributes & FILE_ATTRIBUTE_READONLY) && info.dwVolumeSerialNumber == serial);
    check("final_path", GetFinalPathNameByHandleW(file, path, MAX_PATH, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS) && !wcsncmp(path, L"\\\\?\\D:\\", 7)); /* privacy: allow authored guest path assertion */
    HANDLE mapping = CreateFileMappingW(file, NULL, PAGE_READONLY, 0, 0, NULL);
    check("mapping", mapping != NULL);
    const char *view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
    check("mapped_data", view && !memcmp(view, "hello world", 11));
    DWORD old_protection;
    check("mapping_readonly", !VirtualProtect((void *)view, 4096, PAGE_READWRITE, &old_protection));
    UnmapViewOfFile(view);
    HANDLE copy = CreateFileMappingW(file, NULL, PAGE_WRITECOPY, 0, 0, NULL);
    char *private_view = MapViewOfFile(copy, FILE_MAP_COPY, 0, 0, 0);
    check("private_map", private_view && !memcmp(private_view, "hello world", 11));
    private_view[0] = 'X'; UnmapViewOfFile(private_view); CloseHandle(copy);
    DWORD returned;
    check("sparse_rejected", !DeviceIoControl(file, FSCTL_SET_SPARSE, NULL, 0, NULL, 0, &returned, NULL));
    check("write_rejected", !WriteFile(file, "x", 1, &count, NULL));
    check("create_rejected", CreateFileW(DISC(L"new.bin"), GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL) == INVALID_HANDLE_VALUE);
    check("delete_rejected", !DeleteFileW(DISC(L"ReadMe.txt")));
    check("rename_rejected", !MoveFileW(DISC(L"ReadMe.txt"), DISC(L"other.txt")));
    check("attribute_rejected", !SetFileAttributesW(DISC(L"ReadMe.txt"), FILE_ATTRIBUTE_NORMAL));
    SetFilePointer(file, 0, NULL, FILE_BEGIN);
    check("unchanged", ReadFile(file, bytes, 11, &count, NULL) && count == 11 && !memcmp(bytes, "hello world", 11));
    HANDLE multi = open_read(DISC(L"Sub\\split.bin"));
    SetFilePointer(multi, 3, NULL, FILE_BEGIN);
    check("multi_extent", ReadFile(multi, bytes, sizeof(bytes), &count, NULL) && count == 8 && !memcmp(bytes, "de world", 8));
    CloseHandle(multi);
    HMODULE module = LoadLibraryW(DISC2(L"Probe.dll"));
    DWORD (*value)(void) = module ? (void *)GetProcAddress(module, "VolumeProbe") : NULL;
    check("image_from_disc", value && value() == 0x5151);
    FreeLibrary(module);
    CreateDirectoryW(L"C:\\Program Files\\Volume Probe", NULL);
    check("install_space", GetDiskFreeSpaceExW(L"C:\\Program Files\\Volume Probe", &available, &total, &free_bytes) && total.QuadPart == (256ULL << 30) && available.QuadPart == (64ULL << 30));
    HANDLE installed = CreateFileW(L"C:\\Program Files\\Volume Probe\\file.bin", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    check("install_create", installed != INVALID_HANDLE_VALUE);
    check("install_write", WriteFile(installed, "installed", 9, &count, NULL) && count == 9);
    CloseHandle(installed);
    check("install_rename", MoveFileW(L"C:\\Program Files\\Volume Probe\\file.bin", L"C:\\Program Files\\Volume Probe\\renamed.bin"));
    check("install_copy", CopyFileW(DISC(L"Sub\\\x65e5\x672c.txt"), L"C:\\Program Files\\Volume Probe\\copied.bin", FALSE));
    SetFilePointer(file, 0, NULL, FILE_BEGIN);
    SetHandleInformation(file, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
    SetHandleInformation(mapping, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
    char exe[MAX_PATH], command[MAX_PATH + 80]; STARTUPINFOA startup = {sizeof(startup)}; PROCESS_INFORMATION child;
    lstrcpyA(exe, "G:\\FromDisc.exe"); /* privacy: allow authored guest optical executable */
    snprintf(command, sizeof(command), "\"%s\" --child %lu %lu", exe, (unsigned long)(ULONG_PTR)file, (unsigned long)(ULONG_PTR)mapping);
    check("child", CreateProcessA(NULL, command, NULL, NULL, TRUE, 0, NULL, NULL, &startup, &child));
    /* The child consumes both owners after this process retires. */
    CloseHandle(child.hProcess); CloseHandle(child.hThread); CloseHandle(file); CloseHandle(mapping);
    return 0;
}
