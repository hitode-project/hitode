/* Authored x86 read-only SCM oracle. No service is created, changed or started.
 * Input: the current OS SCM. Output: scalar results only in scm-read-only.bin.
 * A deliberately absent group makes enumeration independent of host contents.
 */
#include <windows.h>
#include <winsvc.h>

static DWORD results[256], used;
static const WCHAR absent_group[] = L"AdvHost_OwnedFixture_NoSuchGroup_729e21";
static const WCHAR absent_service[] = L"AdvHost_OwnedFixture_NoSuchService_729e21";
static const DWORD marker = 0x13572468;
static BYTE buffer[1024];
static BYTE filter_buffer[256 * 1024];
static BYTE config_buffer[8 * 1024];

static void opened(SC_HANDLE handle) {
    results[used++] = handle != NULL;
    results[used++] = GetLastError();
    if (handle) CloseServiceHandle(handle);
}
static void enumeration(SC_HANDLE manager, DWORD level, DWORD type, DWORD state,
                        BYTE *output, DWORD size, DWORD *needed_out,
                        DWORD *returned_out, DWORD *resume_out) {
    DWORD needed = 0xaaaaaaaa, returned = 0xbbbbbbbb, resume = 0;
    BOOL ok;
    if (needed_out) needed_out = &needed;
    if (returned_out) returned_out = &returned;
    if (resume_out) resume_out = &resume;
    SetLastError(marker);
    ok = EnumServicesStatusExW(manager, (SC_ENUM_TYPE)level, type, state,
                              output, size, needed_out, returned_out,
                              resume_out, absent_group);
    results[used++] = ok;
    results[used++] = GetLastError();
    results[used++] = needed;
    results[used++] = returned;
    results[used++] = resume;
}
static void service(SC_HANDLE manager, const WCHAR *name, DWORD access) {
    SC_HANDLE handle;
    SetLastError(marker);
    handle = OpenServiceW(manager, name, access);
    opened(handle);
}
/* Repeated failures verify stable outputs and exercise translated API paths. */
static void repeated_absent_group(SC_HANDLE manager) {
    DWORD index;
    for (index = 0; index < 64; ++index) {
        DWORD needed = 0xaaaaaaaa, returned = 0xbbbbbbbb, resume = 0;
        BOOL ok;
        SetLastError(marker);
        ok = EnumServicesStatusExW(manager, SC_ENUM_PROCESS_INFO, SERVICE_WIN32,
                                  SERVICE_STATE_ALL, NULL, 0, &needed,
                                  &returned, &resume, absent_group);
        if (ok || GetLastError() != ERROR_SERVICE_DOES_NOT_EXIST
            || needed != 0 || returned != 0 || resume != 0) ExitProcess(4);
    }
}
/* Retain only statistics from real rows; never write host service metadata. */
static void filter_statistics(SC_HANDLE manager, DWORD type) {
    DWORD needed, returned, resume = 0, page, index;
    DWORD total = 0, interactive = 0, ordinary = 0, other = 0, error = 0;
    DWORD user = 0, instance = 0;
    BOOL ok = FALSE;
    for (page = 0; page < 64; ++page) {
        ENUM_SERVICE_STATUS_PROCESSW *rows;
        needed = returned = 0;
        SetLastError(marker);
        ok = EnumServicesStatusExW(manager, SC_ENUM_PROCESS_INFO, type,
                                  SERVICE_STATE_ALL, filter_buffer,
                                  sizeof(filter_buffer), &needed, &returned,
                                  &resume, NULL);
        error = GetLastError();
        if (!ok && error != ERROR_MORE_DATA) break;
        rows = (ENUM_SERVICE_STATUS_PROCESSW *)filter_buffer;
        for (index = 0; index < returned; ++index) {
            DWORD actual = rows[index].ServiceStatusProcess.dwServiceType;
            ++total;
            if (actual & 0x40) ++user;
            if (actual & 0x80) ++instance;
            if (!(actual & SERVICE_WIN32)) ++other;
            else if (actual & SERVICE_INTERACTIVE_PROCESS) ++interactive;
            else ++ordinary;
        }
        if (ok) break;
    }
    results[used++] = ok;
    results[used++] = error;
    results[used++] = total;
    results[used++] = interactive;
    results[used++] = ordinary;
    results[used++] = other;
    results[used++] = user;
    results[used++] = instance;
}
/* Read one accessible configuration without retaining its name or contents. */
static void configuration_statistics(SC_HANDLE manager) {
    DWORD needed = 0, returned = 0, resume = 0, index;
    DWORD found = 0, success = 0, error = 0, config_needed = 0;
    DWORD small_success = 0, small_error = 0, small_needed = 0, closed = 0;
    BOOL enumerated = EnumServicesStatusExW(manager, SC_ENUM_PROCESS_INFO,
        SERVICE_WIN32, SERVICE_STATE_ALL, filter_buffer, sizeof(filter_buffer),
        &needed, &returned, &resume, NULL);
    if (enumerated || GetLastError() == ERROR_MORE_DATA) {
        ENUM_SERVICE_STATUS_PROCESSW *rows = (ENUM_SERVICE_STATUS_PROCESSW *)filter_buffer;
        for (index = 0; index < returned; ++index) {
            SC_HANDLE opened = OpenServiceW(manager, rows[index].lpServiceName,
                                           SERVICE_QUERY_CONFIG);
            if (!opened) continue;
            found = 1;
            config_needed = 0xaaaaaaaa;
            SetLastError(marker);
            success = QueryServiceConfigW(opened, (QUERY_SERVICE_CONFIGW *)config_buffer,
                                          sizeof(config_buffer), &config_needed);
            error = GetLastError();
            small_needed = 0xaaaaaaaa;
            SetLastError(marker);
            small_success = QueryServiceConfigW(opened, (QUERY_SERVICE_CONFIGW *)config_buffer,
                                                sizeof(QUERY_SERVICE_CONFIGW) - 1,
                                                &small_needed);
            small_error = GetLastError();
            closed = CloseServiceHandle(opened);
            break;
        }
    }
    results[used++] = found;
    results[used++] = success;
    results[used++] = error;
    results[used++] = config_needed;
    results[used++] = small_success;
    results[used++] = small_error;
    results[used++] = small_needed;
    results[used++] = closed;
}
void mainCRTStartup(void) {
    SC_HANDLE manager, connect;
    HANDLE file; DWORD written, dummy = 1;
    SetLastError(marker); opened(OpenSCManagerW(NULL, NULL, 0));
    SetLastError(marker); opened(OpenSCManagerA("", NULL, SC_MANAGER_ENUMERATE_SERVICE));
    SetLastError(marker); opened(OpenSCManagerW(L".", L"ServicesActive", SC_MANAGER_ENUMERATE_SERVICE));
    SetLastError(marker); opened(OpenSCManagerW(NULL, L"", SC_MANAGER_ENUMERATE_SERVICE));
    SetLastError(marker); opened(OpenSCManagerW(NULL, L"AdvHost_NoSuchDatabase", SC_MANAGER_ENUMERATE_SERVICE));
    SetLastError(marker); opened(OpenSCManagerW(NULL, NULL, 0x40));
    SetLastError(marker); opened(OpenSCManagerW(NULL, NULL, GENERIC_READ));
    manager = OpenSCManagerW(NULL, NULL, SC_MANAGER_ENUMERATE_SERVICE);
    connect = OpenSCManagerW(NULL, NULL, SC_MANAGER_CONNECT);
    if (!manager || !connect) ExitProcess(3);
    repeated_absent_group(manager);
    enumeration(manager, 0, SERVICE_WIN32, SERVICE_STATE_ALL, NULL, 0, &dummy, &dummy, &dummy);
    enumeration(manager, 0, SERVICE_WIN32, SERVICE_STATE_ALL, buffer, sizeof(buffer), &dummy, &dummy, &dummy);
    enumeration(manager, 1, SERVICE_WIN32, SERVICE_STATE_ALL, buffer, sizeof(buffer), &dummy, &dummy, &dummy);
    enumeration(manager, 0, 0, SERVICE_STATE_ALL, buffer, sizeof(buffer), &dummy, &dummy, &dummy);
    enumeration(manager, 0, SERVICE_WIN32, 0, buffer, sizeof(buffer), &dummy, &dummy, &dummy);
    enumeration(connect, 0, SERVICE_WIN32, SERVICE_STATE_ALL, buffer, sizeof(buffer), &dummy, &dummy, &dummy);
    enumeration(NULL, 0, SERVICE_WIN32, SERVICE_STATE_ALL, buffer, sizeof(buffer), &dummy, &dummy, &dummy);
    enumeration(manager, 0, SERVICE_WIN32, SERVICE_STATE_ALL, NULL, 1, &dummy, &dummy, &dummy);
    enumeration(manager, 0, SERVICE_WIN32, SERVICE_STATE_ALL, buffer, sizeof(buffer), NULL, &dummy, &dummy);
    enumeration(manager, 0, SERVICE_WIN32, SERVICE_STATE_ALL, buffer, sizeof(buffer), &dummy, NULL, &dummy);
    enumeration(manager, 0, 0x40, SERVICE_STATE_ALL, buffer, sizeof(buffer), &dummy, &dummy, &dummy);
    enumeration(manager, 0, SERVICE_WIN32 | SERVICE_INTERACTIVE_PROCESS, SERVICE_STATE_ALL, buffer, sizeof(buffer), &dummy, &dummy, &dummy);
    service(manager, absent_service, SERVICE_QUERY_CONFIG);
    service(manager, L"", SERVICE_QUERY_CONFIG);
    service(manager, L"invalid/name", SERVICE_QUERY_CONFIG);
    service(NULL, absent_service, SERVICE_QUERY_CONFIG);
    service(manager, NULL, SERVICE_QUERY_CONFIG);
    SetLastError(marker); results[used++] = QueryServiceConfigW(manager, NULL, 0, &dummy); results[used++] = GetLastError();
    /* Counts may differ across profiles; only filtering relations are checked. */
    filter_statistics(manager, SERVICE_WIN32);
    filter_statistics(manager, SERVICE_WIN32 | SERVICE_INTERACTIVE_PROCESS);
    filter_statistics(manager, SERVICE_TYPE_ALL);
    configuration_statistics(manager);
    SetLastError(marker); results[used++] = CloseServiceHandle(manager); results[used++] = GetLastError();
    SetLastError(marker); results[used++] = CloseServiceHandle(manager); results[used++] = GetLastError();
    SetLastError(marker); results[used++] = CloseServiceHandle(NULL); results[used++] = GetLastError();
    CloseServiceHandle(connect);
    file = CreateFileW(L"scm-read-only.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, results, used * 4, &written, NULL) || written != used * 4) ExitProcess(2);
    CloseHandle(file); ExitProcess(0);
}
