/* The harness supplies enabled backup/restore privileges and a completion reserve.
 * Build: i686-w64-mingw32-gcc -O0 -Wall -o "$ADV_WINE_PROBES/nt_l_process.exe" nt_l_process.c -ladvapi32 */
#define _WIN32_WINNT 0x0602
#include <windows.h>
#include <winternl.h>
#include <stdio.h>

#define NT(name, signature) static NTSTATUS (WINAPI *name) signature
NT(open_tx, (HANDLE *, ACCESS_MASK, OBJECT_ATTRIBUTES *, HANDLE));
NT(open_tx_ex, (HANDLE *, ACCESS_MASK, OBJECT_ATTRIBUTES *, ULONG, HANDLE));
NT(load2, (OBJECT_ATTRIBUTES *, OBJECT_ATTRIBUTES *, ULONG));
NT(load_ex, (OBJECT_ATTRIBUTES *, OBJECT_ATTRIBUTES *, ULONG, HANDLE, HANDLE, ACCESS_MASK, HANDLE *, IO_STATUS_BLOCK *));
NT(clear_event, (HANDLE));
NT(alert_resume, (HANDLE, ULONG *));
NT(queue_ex, (HANDLE, HANDLE, void *, ULONG_PTR, ULONG_PTR, ULONG_PTR));
NT(continue_ex, (CONTEXT *, void *));
NT(next_process, (HANDLE, ACCESS_MASK, ULONG, ULONG, HANDLE *));
NT(suspend_process, (HANDLE));
NT(resume_process, (HANDLE));
NT(query_completion, (HANDLE, ULONG, void *, ULONG, ULONG *));
NT(set_completion_ex, (HANDLE, HANDLE, ULONG_PTR, ULONG_PTR, NTSTATUS, ULONG_PTR));
static LSTATUS (WINAPI *rename_key)(HKEY, const WCHAR *, const WCHAR *);
static void (WINAPI *capture_context)(CONTEXT *);
static int failures;
static volatile ULONG apc_result, continued;

static void check(const char *name, int ok)
{
    printf("%s=%d\n", name, !!ok);
    fflush(stdout);
    if (!ok) ++failures;
}

static void object(OBJECT_ATTRIBUTES *attr, UNICODE_STRING *str, HANDLE root, WCHAR *name)
{
    str->Length = lstrlenW(name) * sizeof(WCHAR);
    str->MaximumLength = str->Length;
    str->Buffer = name;
    InitializeObjectAttributes(attr, str, OBJ_CASE_INSENSITIVE, root, NULL);
}

static int value(HKEY root, const WCHAR *name)
{
    HKEY key;
    DWORD data = 0, size = sizeof(data), type = 0;
    if (RegOpenKeyExW(root, name, 0, KEY_READ, &key)) return 0;
    LSTATUS status = RegQueryValueExW(key, L"answer", NULL, &type, (BYTE *)&data, &size);
    RegCloseKey(key);
    return !status && data == 42 && type == REG_DWORD && size == 4;
}

static void registry_checks(void)
{
    HKEY key = NULL, users = NULL;
    HANDLE opened = NULL;
    DWORD data = 42;
    OBJECT_ATTRIBUTES attr, file;
    UNICODE_STRING name, filename;
    WCHAR hive[MAX_PATH], nt_hive[MAX_PATH + 4];
    GetTempPathW(MAX_PATH, hive);
    lstrcatW(hive, L"nt_l_process.hive");
    wsprintfW(nt_hive, L"\\??\\%s", hive);
    DeleteFileW(hive);
    RegDeleteKeyW(HKEY_CURRENT_USER, L"Software\\HitodeNtProcessRenamed");
    RegDeleteKeyW(HKEY_CURRENT_USER, L"Software\\HitodeNtProcess");
    check("registry_create", !RegCreateKeyExW(HKEY_CURRENT_USER, L"Software\\HitodeNtProcess", 0,
          NULL, 0, KEY_ALL_ACCESS, NULL, &key, NULL) &&
          !RegSetValueExW(key, L"answer", 0, REG_DWORD, (BYTE *)&data, sizeof(data)));
    object(&attr, &name, key, L"");
    check("open_transacted", !open_tx(&opened, KEY_READ, &attr, NULL) && value(opened, L""));
    if (opened) CloseHandle(opened);
    opened = NULL;
    check("open_transacted_ex", !open_tx_ex(&opened, KEY_READ, &attr, 0, NULL) && value(opened, L""));
    if (opened) CloseHandle(opened);
    check("registry_rename", !rename_key(key, NULL, L"HitodeNtProcessRenamed") &&
          value(HKEY_CURRENT_USER, L"Software\\HitodeNtProcessRenamed"));
    check("registry_save", !RegSaveKeyW(key, hive, NULL));
    check("registry_load", !RegLoadKeyW(HKEY_USERS, L"HitodeNtProcessHive", hive) &&
          value(HKEY_USERS, L"HitodeNtProcessHive"));
    check("registry_unload", !RegUnLoadKeyW(HKEY_USERS, L"HitodeNtProcessHive") &&
          RegOpenKeyExW(HKEY_USERS, L"HitodeNtProcessHive", 0, KEY_READ, (HKEY *)&opened) == ERROR_FILE_NOT_FOUND);
    object(&attr, &name, NULL, L"\\Registry\\User");
    open_tx((HANDLE *)&users, KEY_ALL_ACCESS, &attr, NULL);
    object(&attr, &name, users, L"HitodeNtProcessHive");
    object(&file, &filename, NULL, nt_hive);
    check("registry_load2", !load2(&attr, &file, 0) && value(HKEY_USERS, L"HitodeNtProcessHive"));
    RegUnLoadKeyW(HKEY_USERS, L"HitodeNtProcessHive");
    check("registry_load_ex", !load_ex(&attr, &file, 0, NULL, NULL, 0, NULL, NULL) &&
          value(HKEY_USERS, L"HitodeNtProcessHive"));
    RegUnLoadKeyW(HKEY_USERS, L"HitodeNtProcessHive");
    RegCloseKey(users);
    RegCloseKey(key);
    RegDeleteKeyW(HKEY_CURRENT_USER, L"Software\\HitodeNtProcessRenamed");
    DeleteFileW(hive);
}

static DWORD WINAPI worker(void *arg) { return (DWORD)(ULONG_PTR)arg; }
static void WINAPI apc(ULONG_PTR a, ULONG_PTR b, ULONG_PTR c) { apc_result = a + b + c; }

static void continue_check(void *argument)
{
    CONTEXT context;
    continued = 0;
    capture_context(&context);
    if (!continued)
    {
        continued = 1;
        continue_ex(&context, argument);
        check("continue_returned", 0);
    }
}

static void thread_checks(void)
{
    HANDLE event = CreateEventW(NULL, TRUE, TRUE, NULL);
    check("event_clear", !clear_event(event) && WaitForSingleObject(event, 0) == WAIT_TIMEOUT);
    CloseHandle(event);
    DWORD code = 0;
    ULONG count = 0;
    HANDLE thread = CreateThread(NULL, 0, worker, (void *)42, CREATE_SUSPENDED, NULL);
    check("alert_resume", !alert_resume(thread, &count) && count == 1 &&
          WaitForSingleObject(thread, INFINITE) == WAIT_OBJECT_0 &&
          GetExitCodeThread(thread, &code) && code == 42);
    CloseHandle(thread);
    check("queue_apc_ex", !queue_ex(GetCurrentThread(), NULL, apc, 10, 20, 12) &&
          SleepEx(0, TRUE) == WAIT_IO_COMPLETION && apc_result == 42);
    continue_check(NULL);
    check("continue_ex", continued == 1);
    ULONG argument[6] = {0, 1, 0, 0, 0, 0};
    apc_result = 0;
    queue_ex(GetCurrentThread(), NULL, apc, 1, 2, 3);
    continue_check(argument);
    check("continue_ex_alert", continued == 1 && apc_result == 6);
    SYSTEM_INFO info;
    GetSystemInfo(&info);
    check("processor_number", GetCurrentProcessorNumber() < info.dwNumberOfProcessors);
    HANDLE limited = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, GetCurrentProcessId());
    check("process_suspend_access", (ULONG)suspend_process(limited) == 0xc0000022);
    CloseHandle(limited);
    check("process_suspend_invalid", (ULONG)suspend_process((HANDLE)0x12345678) == 0xc0000008);
    thread = CreateThread(NULL, 0, worker, (void *)43, CREATE_SUSPENDED, NULL);
    HANDLE resume_handle = OpenProcess(PROCESS_SUSPEND_RESUME, FALSE, GetCurrentProcessId());
    check("process_resume", !resume_process(resume_handle) &&
          WaitForSingleObject(thread, INFINITE) == WAIT_OBJECT_0 &&
          GetExitCodeThread(thread, &code) && code == 43);
    CloseHandle(thread);
    CloseHandle(resume_handle);
    HANDLE process = NULL, next = NULL;
    NTSTATUS status;
    int found = 0;
    while (!(status = next_process(process, PROCESS_QUERY_INFORMATION, 0, 0, &next)))
    {
        if (GetProcessId(next) == GetCurrentProcessId()) found = 1;
        if (process) CloseHandle(process);
        process = next;
    }
    if (process) CloseHandle(process);
    check("process_next", found && (ULONG)status == 0x8000001a && !next);
}

static HANDLE job_checks(void)
{
    HANDLE port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, NULL, 0, 1);
    DWORD bytes = 0, returned = 0;
    ULONG_PTR key = 0;
    OVERLAPPED *overlapped = NULL;
    ULONG depth = 0;
    PostQueuedCompletionStatus(port, 42, 0x1234, (OVERLAPPED *)0x5678);
    check("completion_query", !query_completion(port, 0, &depth, sizeof(depth), &returned) &&
          depth == 1 && returned == 4 && GetQueuedCompletionStatus(port, &bytes, &key, &overlapped, 0) &&
          bytes == 42 && key == 0x1234 && overlapped == (OVERLAPPED *)0x5678);
    check("completion_ex_reserve", (ULONG)set_completion_ex(port, NULL, 1, 2, 0, 3) == 0xc0000008 &&
          (ULONG)set_completion_ex(port, (HANDLE)0x12345678, 1, 2, 0, 3) == 0xc0000008 &&
          !query_completion(port, 0, &depth, sizeof(depth), NULL) && depth == 0);
    HANDLE reserve = NULL;
    HANDLE fixture = CreateFileW(L"nt_l_reserve.bin", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    DWORD read = 0;
    BOOL supplied = ReadFile(fixture, &reserve, sizeof(reserve), &read, NULL) && read == sizeof(reserve);
    CloseHandle(fixture);
    DeleteFileW(L"nt_l_reserve.bin");
    check("completion_ex_post", supplied && !set_completion_ex(port, reserve, 0x2468, 0x1357, 0, 43) &&
          !query_completion(port, 0, &depth, sizeof(depth), NULL) && depth == 1 &&
          GetQueuedCompletionStatus(port, &bytes, &key, &overlapped, 0) && bytes == 43 &&
          key == 0x2468 && overlapped == (OVERLAPPED *)0x1357);
    CloseHandle(reserve);
    HANDLE job = CreateJobObjectW(NULL, NULL), empty = CreateJobObjectW(NULL, NULL);
    BOOL in_job = TRUE;
    check("job_membership_before", IsProcessInJob(GetCurrentProcess(), job, &in_job) && !in_job);
    JOBOBJECT_BASIC_LIMIT_INFORMATION basic = {0};
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION extended = {0};
    check("job_set_limits", SetInformationJobObject(job, JobObjectBasicLimitInformation, &basic, sizeof(basic)) &&
          SetInformationJobObject(job, JobObjectExtendedLimitInformation, &extended, sizeof(extended)));
    JOBOBJECT_ASSOCIATE_COMPLETION_PORT association = {(void *)0x4321, port};
    check("job_completion", SetInformationJobObject(job, JobObjectAssociateCompletionPortInformation,
          &association, sizeof(association)));
    check("job_assign", AssignProcessToJobObject(job, GetCurrentProcess()) &&
          IsProcessInJob(GetCurrentProcess(), job, &in_job) && in_job);
    check("job_notification", GetQueuedCompletionStatus(port, &bytes, &key, &overlapped, 0) &&
          bytes == JOB_OBJECT_MSG_NEW_PROCESS && key == 0x4321 &&
          (ULONG_PTR)overlapped == GetCurrentProcessId());
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting;
    check("job_accounting", QueryInformationJobObject(job, JobObjectBasicAccountingInformation,
          &accounting, sizeof(accounting), &returned) && returned == sizeof(accounting) &&
          accounting.TotalProcesses == 1 && accounting.ActiveProcesses == 1);
    JOBOBJECT_BASIC_PROCESS_ID_LIST list;
    check("job_process_ids", QueryInformationJobObject(job, JobObjectBasicProcessIdList, &list, sizeof(list), &returned) &&
          list.NumberOfAssignedProcesses == 1 && list.NumberOfProcessIdsInList == 1 &&
          list.ProcessIdList[0] == GetCurrentProcessId());
    check("job_query_limits", QueryInformationJobObject(job, JobObjectBasicLimitInformation, &basic, sizeof(basic), NULL) &&
          QueryInformationJobObject(job, JobObjectExtendedLimitInformation, &extended, sizeof(extended), NULL));
    check("job_terminate_empty", TerminateJobObject(empty, 0));
    CloseHandle(empty);
    CloseHandle(port);
    return job;
}

int main(void)
{
    HMODULE nt = GetModuleHandleW(L"ntdll.dll");
#define LOAD(var, name) var = (void *)GetProcAddress(nt, name)
    LOAD(open_tx, "NtOpenKeyTransacted"); LOAD(open_tx_ex, "NtOpenKeyTransactedEx");
    LOAD(load2, "NtLoadKey2"); LOAD(load_ex, "NtLoadKeyEx");
    LOAD(clear_event, "NtClearEvent"); LOAD(alert_resume, "NtAlertResumeThread");
    LOAD(queue_ex, "NtQueueApcThreadEx"); LOAD(continue_ex, "NtContinueEx");
    LOAD(next_process, "NtGetNextProcess"); LOAD(suspend_process, "NtSuspendProcess");
    LOAD(resume_process, "NtResumeProcess"); LOAD(query_completion, "NtQueryIoCompletion");
    LOAD(set_completion_ex, "NtSetIoCompletionEx"); LOAD(capture_context, "RtlCaptureContext");
    rename_key = (void *)GetProcAddress(GetModuleHandleW(L"advapi32.dll"), "RegRenameKey");
    registry_checks();
    thread_checks();
    HANDLE job = job_checks();
    HANDLE terminate = NULL;
    BOOL duplicated = DuplicateHandle(GetCurrentProcess(), job, GetCurrentProcess(), &terminate,
                                      JOB_OBJECT_TERMINATE, FALSE, 0);
    CloseHandle(job);
    check("job_terminate_self", !failures && duplicated);
    TerminateJobObject(terminate, failures ? 1 : 37);
    puts("job_terminate_returned=1");
    return 1;
}
