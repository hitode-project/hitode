/* Build: i686-w64-mingw32-gcc -O1 -nostdlib -Wl,--entry,_mainCRTStartup -o nt_j_file.exe nt_j_file.c -lkernel32 -lntdll */
#include <windows.h>
#include <winternl.h>
#include <winioctl.h>

/* These two NT calls have no Win32 wrapper in Wine 11. */
typedef NTSTATUS (WINAPI *flush_ex_fn)(HANDLE, ULONG, void *, ULONG, IO_STATUS_BLOCK *);
typedef NTSTATUS (WINAPI *delete_fn)(OBJECT_ATTRIBUTES *);
typedef NTSTATUS (WINAPI *directory_fn)(HANDLE, HANDLE, void *, void *, IO_STATUS_BLOCK *, void *, ULONG, ULONG, ULONG, UNICODE_STRING *, ULONG);
NTSYSAPI void WINAPI RtlFreeUnicodeString(UNICODE_STRING *);
static HANDLE file, ready, pipe_read, notify_directory;
static OVERLAPPED notify_io;
static DWORD written;
static int failed;

static void check(const char *line, BOOL ok)
{
    DWORD length = lstrlenA(line);
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), line, length, &written, NULL);
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), ok ? "=ok\n" : "=FAIL\n", ok ? 4 : 6, &written, NULL);
    if (!ok) failed = 1;
}

static DWORD WINAPI lock_worker(void *arg)
{
    OVERLAPPED ov = {0};
    SetEvent(ready);
    if (!LockFileEx(file, LOCKFILE_EXCLUSIVE_LOCK, 0, 4, 0, &ov)) return 1;
    return !UnlockFileEx(file, 0, 4, 0, &ov);
}

static DWORD WINAPI pipe_worker(void *arg)
{
    char bytes[4];
    DWORD count;
    SetEvent(ready);
    if (!ReadFile(pipe_read, bytes, sizeof(bytes), &count, NULL)) return 1;
    return count != 4 || bytes[0] != 'p' || bytes[3] != 'e';
}

static DWORD WINAPI cancel_worker(void *arg)
{
    WaitForSingleObject(ready, INFINITE);
    return !CancelIoEx(notify_directory, &notify_io);
}

static void enumerate(void)
{
    const WCHAR *names[] = {L"j-enum/part00.dat", L"j-enum/part01.dat", L"j-enum/part02.dat"};
    BYTE buffer[1024];
    IO_STATUS_BLOCK io;
    UNICODE_STRING mask;
    HANDLE file, directory, search;
    WIN32_FIND_DATAW found;
    DWORD count = 0;
    NTSTATUS status;
    directory_fn query = (directory_fn)GetProcAddress(GetModuleHandleA("ntdll.dll"), "NtQueryDirectoryFile");
    CreateDirectoryW(L"j-enum", NULL);
    for (int i = 0; i < 3; i++) {
        file = CreateFileW(names[i], GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, 0, NULL);
        CloseHandle(file);
    }
    search = FindFirstFileW(L"j-enum/part??.dat", &found);
    if (search != INVALID_HANDLE_VALUE) {
        do { count++; } while (count < 4 && FindNextFileW(search, &found));
        FindClose(search);
    }
    check("directory_dos_mask", count == 3);
    directory = CreateFileW(L"j-enum", FILE_LIST_DIRECTORY, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                            NULL, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
    RtlInitUnicodeString(&mask, L"part>>.dat");
    status = query(directory, NULL, NULL, NULL, &io, buffer, sizeof(buffer), 1, 0xfeed0000, &mask, 0xbeef0001);
    count = 0;
    if (!status) {
        BYTE *entry = buffer;
        do { count++; if (!*(ULONG *)entry) break; entry += *(ULONG *)entry; } while (count < 4);
    }
    check("directory_boolean_batch", status == 0 && count == 3);
    status = query(directory, NULL, NULL, NULL, &io, buffer, sizeof(buffer), 1, 0xfeed0000, NULL, 0xbeef0000);
    check("directory_boolean_resume", status == (NTSTATUS)0x80000006);
    status = query(directory, NULL, NULL, NULL, &io, buffer, sizeof(buffer), 1, 0xfeed0001, NULL, 0xbeef0001);
    check("directory_boolean_single", status == 0 && *(ULONG *)buffer == 0 && io.Information < 100);
    CloseHandle(directory);
    for (int i = 0; i < 3; i++) DeleteFileW(names[i]);
    RemoveDirectoryW(L"j-enum");
}

static void run(void)
{
    HANDLE h, other, worker, directory, slot, pipe_write;
    DWORD count, code, returned;
    BOOL result;
    OVERLAPPED ov = {0};
    char buffer[1024];
    WCHAR full[MAX_PATH];
    IO_STATUS_BLOCK io;
    OBJECT_ATTRIBUTES attr;
    UNICODE_STRING name;
    HMODULE ntdll = GetModuleHandleA("ntdll.dll");
    flush_ex_fn flush_ex = (flush_ex_fn)GetProcAddress(ntdll, "NtFlushBuffersFileEx");
    delete_fn delete_file = (delete_fn)GetProcAddress(ntdll, "NtDeleteFile");
    FILE_SEGMENT_ELEMENT segments[3] = {{0}};
    BYTE *pages;

    h = CreateFileW(L"j-file.tmp", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    check("flush_file", h != INVALID_HANDLE_VALUE && WriteFile(h, "data", 4, &count, NULL) && count == 4 && FlushFileBuffers(h));
    check("flush_ex", flush_ex(h, 0, NULL, 0, &io) == 0 && io.Status == 0 && io.Information == 0);
    other = CreateFileW(L"j-file.tmp", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    result = FlushFileBuffers(other);
    check("flush_access", !result && GetLastError() == ERROR_ACCESS_DENIED);
    CloseHandle(other);

    check("lock", LockFile(h, 0, 0, 4, 0));
    file = CreateFileW(L"j-file.tmp", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                      NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    result = LockFile(file, 0, 0, 4, 0);
    check("lock_conflict", !result && GetLastError() == ERROR_LOCK_VIOLATION);
    ready = CreateEventW(NULL, FALSE, FALSE, NULL);
    worker = CreateThread(NULL, 0, lock_worker, NULL, 0, NULL);
    WaitForSingleObject(ready, INFINITE);
    check("lock_wait", WaitForSingleObject(worker, 10) == WAIT_TIMEOUT);
    check("unlock", UnlockFile(h, 0, 0, 4, 0));
    WaitForSingleObject(worker, INFINITE);
    GetExitCodeThread(worker, &code);
    check("lock_woken", code == 0);
    CloseHandle(worker);
    CloseHandle(file);
    CloseHandle(h);

    GetFullPathNameW(L"j-file.tmp", MAX_PATH, full, NULL);
    RtlDosPathNameToNtPathName_U(full, &name, NULL, NULL);
    InitializeObjectAttributes(&attr, &name, OBJ_CASE_INSENSITIVE, NULL, NULL);
    check("delete", delete_file(&attr) == 0 && GetFileAttributesW(full) == INVALID_FILE_ATTRIBUTES && GetLastError() == ERROR_FILE_NOT_FOUND);
    RtlFreeUnicodeString(&name);

    slot = CreateMailslotW(L"\\\\.\\mailslot\\j-file", 64, MAILSLOT_WAIT_FOREVER, NULL);
    check("mailslot", slot != INVALID_HANDLE_VALUE && CloseHandle(slot));

    CreateDirectoryW(L"j-notify", NULL);
    directory = CreateFileW(L"j-notify", FILE_LIST_DIRECTORY, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                           NULL, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED, NULL);
    notify_directory = directory;
    notify_io.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    result = ReadDirectoryChangesW(directory, buffer, sizeof(buffer), FALSE, 0, &returned, &notify_io, NULL);
    check("notify_filter", !result && GetLastError() == ERROR_INVALID_PARAMETER);
    check("cancel", CancelIo(directory));
    check("notify", ReadDirectoryChangesW(directory, buffer, sizeof(buffer), TRUE, FILE_NOTIFY_CHANGE_FILE_NAME, &returned, &notify_io, NULL));
    worker = CreateThread(NULL, 0, cancel_worker, NULL, 0, NULL);
    SignalObjectAndWait(ready, notify_io.hEvent, INFINITE, FALSE);
    WaitForSingleObject(worker, INFINITE);
    GetExitCodeThread(worker, &code);
    check("cancel_ex", code == 0);
    result = GetOverlappedResult(directory, &notify_io, &returned, FALSE);
    check("cancel_ex_result", !result && GetLastError() == ERROR_OPERATION_ABORTED);
    CloseHandle(worker);
    result = CancelSynchronousIo(GetCurrentThread());
    check("cancel_sync", !result && GetLastError() == ERROR_NOT_FOUND);
    CloseHandle(notify_io.hEvent);
    CloseHandle(directory);
    RemoveDirectoryW(L"j-notify");

    h = CreateFileW(L"j-scatter.tmp", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS,
                    FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED, NULL);
    pages = VirtualAlloc(NULL, 8192, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    segments[0].Buffer = (PVOID64)(ULONG_PTR)pages;
    segments[1].Buffer = (PVOID64)(ULONG_PTR)(pages + 4096);
    pages[0] = 0x12; pages[4095] = 0x34; pages[4096] = 0x56; pages[8191] = 0x78;
    ov = (OVERLAPPED){0};
    ov.hEvent = CreateEventW(NULL, TRUE, FALSE, NULL);
    result = WriteFileGather(h, segments, 8192, NULL, &ov);
    check("gather", (result || GetLastError() == ERROR_IO_PENDING) && GetOverlappedResult(h, &ov, &returned, TRUE) && returned == 8192);
    pages[0] = pages[4095] = pages[4096] = pages[8191] = 0;
    ResetEvent(ov.hEvent);
    result = ReadFileScatter(h, segments, 8192, NULL, &ov);
    check("scatter", (result || GetLastError() == ERROR_IO_PENDING) && GetOverlappedResult(h, &ov, &returned, TRUE) && returned == 8192 &&
          pages[0] == 0x12 && pages[4095] == 0x34 && pages[4096] == 0x56 && pages[8191] == 0x78);
    result = DeviceIoControl(INVALID_HANDLE_VALUE, IOCTL_DISK_GET_DRIVE_GEOMETRY, NULL, 0, buffer, sizeof(buffer), &returned, NULL);
    check("device_invalid", !result && GetLastError() == ERROR_INVALID_HANDLE);
    result = DeviceIoControl(h, IOCTL_DISK_GET_DRIVE_GEOMETRY, NULL, 0, buffer, sizeof(buffer), &returned, &ov);
    check("device_disk", !result);
    result = DeviceIoControl(h, 0x222000, NULL, 0, buffer, sizeof(buffer), &returned, &ov);
    check("device_server", !result && (GetLastError() == ERROR_NOT_SUPPORTED || GetLastError() == ERROR_INVALID_FUNCTION));
    CloseHandle(ov.hEvent);
    CloseHandle(h);
    VirtualFree(pages, 0, MEM_RELEASE);
    DeleteFileW(L"j-scatter.tmp");

    CreatePipe(&pipe_read, &pipe_write, NULL, 0);
    WriteFile(pipe_write, "pipe", 4, &count, NULL);
    worker = CreateThread(NULL, 0, pipe_worker, NULL, 0, NULL);
    WaitForSingleObject(ready, INFINITE);
    check("flush_pipe", FlushFileBuffers(pipe_write));
    WaitForSingleObject(worker, INFINITE);
    GetExitCodeThread(worker, &code);
    check("pipe_read", code == 0);
    CloseHandle(worker);
    CloseHandle(pipe_read);
    CloseHandle(pipe_write);
    CloseHandle(ready);
}

void mainCRTStartup(void)
{
    enumerate();
    run();
    ExitProcess(failed);
}
