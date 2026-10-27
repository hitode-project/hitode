/* i686-w64-mingw32-gcc -O2 -Wall -o "$ADV_WINE_PROBES/nt_m_system.exe" nt_m_system.c -ladvapi32 */
#define _WIN32_WINNT 0x0a00
#include <windows.h>
#include <winternl.h>
#include <stdio.h>
#include <string.h>

static unsigned failures;
static void check(const char *name, int passed)
{
    printf("%s: %s\n", name, passed ? "ok" : "FAIL");
    fflush(stdout);
    failures += !passed;
}

/* Direct NT calls cover services without kernel32/advapi32 wrappers. */
typedef NTSTATUS (WINAPI *license_fn)(UNICODE_STRING *, ULONG *, void *, ULONG, ULONG *);
typedef NTSTATUS (WINAPI *set_locale_fn)(BOOLEAN, LCID);
typedef NTSTATUS (WINAPI *query_locale_fn)(BOOLEAN, LCID *);
typedef NTSTATUS (WINAPI *alloc64_fn)(HANDLE, ULONGLONG *, ULONGLONG, ULONGLONG *, ULONG, ULONG);
typedef NTSTATUS (WINAPI *query64_fn)(HANDLE, ULONG, void *, ULONG, ULONG *);
typedef NTSTATUS (WINAPI *transfer64_fn)(HANDLE, ULONGLONG, void *, ULONGLONG, ULONGLONG *);
typedef HANDLE (WINAPI *mapping2_fn)(HANDLE, SECURITY_ATTRIBUTES *, ULONG, ULONG, ULONG, ULONG64, LPCWSTR, void *, ULONG);
typedef void *(WINAPI *map3_fn)(HANDLE, HANDLE, void *, ULONG64, SIZE_T, ULONG, ULONG, void *, ULONG);
typedef BOOL (WINAPI *unmap2_fn)(HANDLE, void *, ULONG);
typedef BOOL (WINAPI *prefetch_fn)(HANDLE, ULONG_PTR, void *, ULONG);
typedef void *(WINAPI *alloc2_fn)(HANDLE, void *, SIZE_T, ULONG, ULONG, void *, ULONG);

int main(void)
{
    HMODULE nt = GetModuleHandleA("ntdll.dll"), kb = GetModuleHandleA("kernelbase.dll");
    mapping2_fn mapping2 = (mapping2_fn)GetProcAddress(kb, "CreateFileMapping2");
    map3_fn map3 = (map3_fn)GetProcAddress(kb, "MapViewOfFile3");
    unmap2_fn unmap2 = (unmap2_fn)GetProcAddress(kb, "UnmapViewOfFile2");
    prefetch_fn prefetch = (prefetch_fn)GetProcAddress(kb, "PrefetchVirtualMemory");
    alloc2_fn alloc2 = (alloc2_fn)GetProcAddress(kb, "VirtualAlloc2");
    HANDLE mapping, file, process;
    char *view, *other, *allocation;
    DWORD value, kind, length;
    char filename[MAX_PATH], temp[MAX_PATH];
    struct { void *address; SIZE_T size; } range;
    MEMORY_BASIC_INFORMATION mbi;
    void *placeholder;

    mapping = mapping2(INVALID_HANDLE_VALUE, NULL, SECTION_ALL_ACCESS, PAGE_READWRITE, SEC_COMMIT, 65536, NULL, NULL, 0);
    check("create-section-ex", mapping != NULL);
    view = map3(mapping, GetCurrentProcess(), NULL, 0, 65536, 0, PAGE_READWRITE, NULL, 0);
    other = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 65536);
    if (!view || !other) return 2;
    memcpy(view, "section", 8);
    check("map-view-ex-shared", !memcmp(other, "section", 8));
    check("map-view-ex-alignment", !map3(mapping, NULL, NULL, 1, 4096, 0, PAGE_READONLY, NULL, 0)
        && GetLastError() == ERROR_MAPPED_ALIGNMENT);
    range.address = view; range.size = 4096;
    check("prefetch-memory", prefetch(GetCurrentProcess(), 1, &range, 0));
    check("shared-virtual-lock", VirtualLock(view, 32));
    check("shared-virtual-unlock", VirtualUnlock(view, 32));
    check("unmap-view-ex", unmap2(GetCurrentProcess(), view, 0));
    UnmapViewOfFile(other); CloseHandle(mapping);

    placeholder = alloc2(GetCurrentProcess(), NULL, 65536, MEM_RESERVE | 0x40000, PAGE_NOACCESS, NULL, 0);
    mapping = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 65536, NULL);
    view = map3(mapping, NULL, placeholder, 0, 65536, 0x4000, PAGE_READWRITE, NULL, 0);
    check("map-view-ex-placeholder", placeholder && view == placeholder);
    check("unmap-preserve-placeholder", view && unmap2(GetCurrentProcess(), view, 2)
        && VirtualQuery(placeholder, &mbi, sizeof(mbi)) == sizeof(mbi) && mbi.State == MEM_RESERVE);
    VirtualFree(placeholder, 0, MEM_RELEASE); CloseHandle(mapping);

    GetTempPathA(sizeof(temp), temp); GetTempFileNameA(temp, "ntm", 0, filename);
    file = CreateFileA(filename, GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    mapping = CreateFileMappingW(file, NULL, PAGE_READWRITE, 0, 65536, NULL);
    view = MapViewOfFile(mapping, FILE_MAP_WRITE, 0, 0, 0);
    if (!view) return 3;
    memcpy(view, "flushed", 8);
    check("flush-view", FlushViewOfFile(view + 1, 7));
    UnmapViewOfFile(view); CloseHandle(mapping); SetFilePointer(file, 0, NULL, FILE_BEGIN);
    { char data[8]; DWORD got;
      check("flush-file-data", ReadFile(file, data, 8, &got, NULL) && got == 8 && !memcmp(data, "flushed", 8)); }
    CloseHandle(file); DeleteFileA(filename);

    allocation = VirtualAlloc(NULL, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    check("virtual-lock", allocation && VirtualLock(allocation + 1, 32));
    check("virtual-unlock", VirtualUnlock(allocation + 1, 32));
    FlushProcessWriteBuffers(); check("flush-write-buffers", allocation != NULL);
    VirtualFree(allocation, 0, MEM_RELEASE);
    value = SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
    check("execution-state-set", value != 0);
    check("execution-state-previous", SetThreadExecutionState(ES_CONTINUOUS) == (ES_CONTINUOUS | ES_SYSTEM_REQUIRED));

    {
        set_locale_fn set = (set_locale_fn)GetProcAddress(nt, "NtSetDefaultLocale");
        query_locale_fn query = (query_locale_fn)GetProcAddress(nt, "NtQueryDefaultLocale");
        LCID system, user, current;
        query(FALSE, &system); query(TRUE, &user);
        check("set-default-locale", !set(TRUE, 0x411) && !query(TRUE, &current) && current == 0x411);
        check("system-locale-isolation", !query(FALSE, &current) && current == system);
        check("set-system-locale", !set(FALSE, 0x804) && !query(FALSE, &current) && current == 0x804);
        check("user-locale-isolation", !query(TRUE, &current) && current == 0x411);
        set(FALSE, system); set(TRUE, user);
    }
    {
        license_fn license = (license_fn)GetProcAddress(nt, "NtQueryLicenseValue");
        HKEY key;
        WCHAR name[] = L"MSystemProbe";
        UNICODE_STRING string = {sizeof(name) - sizeof(WCHAR), sizeof(name), name};
        NTSTATUS status;
        value = 0x13572468;
        if (RegCreateKeyExW(HKEY_LOCAL_MACHINE, L"Software\\Wine\\LicenseInformation", 0, NULL, 0, KEY_SET_VALUE, NULL, &key, NULL)) return 4;
        RegSetValueExW(key, name, 0, REG_DWORD, (BYTE *)&value, 4);
        status = license(&string, &kind, NULL, 0, &length);
        check("license-size", status == (NTSTATUS)0xc0000023 && kind == REG_DWORD && length == 4);
        value = 0;
        check("license-registry-data", !license(&string, &kind, &value, 4, &length) && value == 0x13572468);
        RegDeleteValueW(key, name); RegCloseKey(key);
    }
    {
        alloc64_fn alloc = (alloc64_fn)GetProcAddress(nt, "NtWow64AllocateVirtualMemory64");
        query64_fn query = (query64_fn)GetProcAddress(nt, "NtWow64QueryInformationProcess64");
        transfer64_fn read = (transfer64_fn)GetProcAddress(nt, "NtWow64ReadVirtualMemory64");
        transfer64_fn write = (transfer64_fn)GetProcAddress(nt, "NtWow64WriteVirtualMemory64");
        ULONGLONG base = 0, size = 4096, count = 99;
        BYTE basic[48];
        ULONGLONG empty = 0;
        check("wow64-allocate-validation", alloc(GetCurrentProcess(), NULL, 0, &empty, MEM_COMMIT, PAGE_READWRITE) == (NTSTATUS)0xc00000f2
            && alloc(GetCurrentProcess(), NULL, 22, &size, MEM_COMMIT, PAGE_READWRITE) == (NTSTATUS)0xc00000f1);
        check("wow64-allocate", !alloc(GetCurrentProcess(), &base, 0, &size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) && base && size == 4096);
        VirtualFree((void *)(ULONG_PTR)base, 0, MEM_RELEASE);
        check("wow64-query-process", !query(GetCurrentProcess(), 0, basic, 48, &length) && length == 48 && *(ULONGLONG *)(basic + 32) == GetCurrentProcessId());
        check("wow64-read-empty", !read(GetCurrentProcess(), 0, NULL, 0, &count) && count == 0);
        count = 99;
        check("wow64-write-empty", !write(GetCurrentProcess(), 0, NULL, 0, &count) && count == 0);
        process = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, GetCurrentProcessId());
        check("wow64-read-access", read(process, 0, &value, 4, &count) == (NTSTATUS)0xc0000022 && count == 0);
        count = 99;
        check("wow64-write-access", write(process, 0, &value, 4, &count) == (NTSTATUS)0xc0000022 && count == 0);
        CloseHandle(process);
    }
    check("debug-attach-self", !DebugActiveProcess(GetCurrentProcessId()));
    check("debug-kill-on-exit", DebugSetProcessKillOnExit(FALSE));
    check("debug-continue-invalid", !ContinueDebugEvent(GetCurrentProcessId(), GetCurrentThreadId(), DBG_CONTINUE));
    check("debug-detach-unattached", !DebugActiveProcessStop(GetCurrentProcessId()));
    { DEBUG_EVENT event;
      check("debug-wait-timeout", !WaitForDebugEvent(&event, 1) && GetLastError() == ERROR_SEM_TIMEOUT); }
    return failures ? 1 : 0;
}
