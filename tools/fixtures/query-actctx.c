/* Authored x86 default QueryActCtxW oracle; no game input or pointer output.
 * query_actctx.py builds both an unbound executable and a manifest control.
 * Output: result.bin, 16 header DWORDs followed by 25-DWORD scalar rows.
 */
#include <windows.h>

typedef BOOL (WINAPI *QUERY)(DWORD, HANDLE, PVOID, ULONG, PVOID, SIZE_T, SIZE_T *);
static QUERY query;
static DWORD output[4096], used = 16;
static BYTE bytes[256];

static void put(DWORD value) { output[used++] = value; }
static void one(DWORD id, DWORD flags, HANDLE handle, DWORD kind, DWORD cls,
                DWORD size, DWORD has_buffer, DWORD has_required, DWORD subkind) {
    DWORD index, error, good, unchanged = 1, tail = 1;
    DWORD sub[2] = {subkind == 2 ? 1 : 0, 0};
    SIZE_T required = 0xcccccccc;
    DWORD *words = (DWORD *)bytes;
    for (index = 0; index < sizeof(bytes); index++) bytes[index] = 0xcc;
    SetLastError(0x12345678);
    good = query(flags, handle, subkind ? sub : NULL, cls,
                 has_buffer ? bytes : NULL, size, has_required ? &required : NULL);
    error = GetLastError();
    for (index = 0; index < sizeof(bytes); index++)
        if (bytes[index] != 0xcc) unchanged = 0;
    if (required <= sizeof(bytes))
        for (index = (DWORD)required; index < sizeof(bytes); index++)
            if (bytes[index] != 0xcc) tail = 0;
    put(id); put(flags); put(kind); put(cls); put(size);
    put(has_buffer); put(has_required); put(subkind);
    put(good); put(error); put((DWORD)required); put(unchanged); put(tail);
    for (index = 0; index < 12; index++) {
        DWORD value = words[index];
        if (good && ((cls == 1 && index == 0) || (cls == 2 && index >= 9)))
            value = value == 0;
        if (!good) value = value == 0xcccccccc;
        put(value);
    }
    if (good && cls == 1 && words[0] && !(flags & QUERY_ACTCTX_FLAG_NO_ADDREF))
        ReleaseActCtx((HANDLE)(UINT_PTR)words[0]);
}

void mainCRTStartup(void) {
    DWORD id = 0, index, cls, mode;
    DWORD classes[4] = {1, 2, 5, 6};
    DWORD flags[4] = {0, 4, 0x80000000, 0x80000004};
    DWORD sizes[7] = {0, 1, 4, 7, 8, 12, 48};
    DWORD other[9] = {1, 2, 0x20, 12, 20, 24, 0x80000008, 0x80000010, 0xffffffff};
    DWORD legacy[5] = {1, 5, 9, 17, 0x80000001};
    HANDLE current = (HANDLE)(UINT_PTR)0xcccccccc, file;
    ULONG_PTR cookie = 0;
    DWORD written;
    HMODULE main_module = GetModuleHandleW(NULL);
    OSVERSIONINFOW os = {0};
    query = (QUERY)GetProcAddress(GetModuleHandleW(L"kernel32.dll"), "QueryActCtxW");
    SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    os.dwOSVersionInfoSize = sizeof(os);
    /* Advisory compatibility version, independent of the queried SxS contract. */
    GetVersionExW(&os);
    output[0] = 1; output[1] = os.dwMajorVersion;
    output[2] = os.dwMinorVersion; output[3] = os.dwBuildNumber;
    SetLastError(0x12345678);
    output[4] = GetCurrentActCtx(&current); output[5] = current == NULL;
    output[6] = current == INVALID_HANDLE_VALUE; output[7] = GetLastError();
    output[8] = sizeof(ACTIVATION_CONTEXT_BASIC_INFORMATION);
    output[9] = sizeof(ACTIVATION_CONTEXT_DETAILED_INFORMATION);
    output[10] = sizeof(ACTIVATION_CONTEXT_RUN_LEVEL_INFORMATION);
    output[11] = query != NULL;
    if (!query) ExitProcess(2);
    for (index = 0; index < 4; index++) for (mode = 0; mode < 4; mode++) {
        cls = classes[mode];
        one(id++, flags[index], NULL, 0, cls, 256, 1, 1, 0);
        one(id++, flags[index], NULL, 0, cls, 0, 0, 1, 0);
    }
    for (index = 0; index < 4; index++) for (mode = 0; mode < 7; mode++)
        one(id++, 0, NULL, 0, classes[index], sizes[mode], 1, 1, 0);
    for (index = 0; index < 4; index++) {
        cls = classes[index];
        one(id++, 0, NULL, 0, cls, 256, 1, 0, 0);
        one(id++, 4, main_module, 2, cls, 256, 1, 1, 0);
        one(id++, 8, main_module, 2, cls, 256, 1, 1, 0);
        one(id++, 16, (HANDLE)&mainCRTStartup, 3, cls, 256, 1, 1, 0);
        one(id++, 0, NULL, 0, cls, 256, 1, 1, 1);
        one(id++, 0, NULL, 0, cls, 256, 1, 1, 2);
    }
    for (mode = 0; mode < 9; mode++)
        one(id++, other[mode], (other[mode] & 24) ? main_module : NULL,
            (other[mode] & 24) ? 2 : 0, 1, 256, 1, 1, 0);
    one(id++, 0, NULL, 0, 1, 8, 0, 1, 0);
    one(id++, 0, NULL, 0, 1, 0, 0, 0, 0);
    for (index = 0; index < 4; index++) {
        cls = classes[index];
        one(id++, 0, NULL, 0, cls, 0, 0, 0, 0);
        one(id++, 0, NULL, 0, cls, 8, 0, 1, 0);
        one(id++, 8, NULL, 0, cls, 256, 1, 1, 0);
        one(id++, 16, NULL, 0, cls, 256, 1, 1, 0);
    }
    for (mode = 0; mode < 5; mode++)
        one(id++, legacy[mode], (legacy[mode] & 24) ? main_module : NULL,
            (legacy[mode] & 24) ? 2 : 0, 1, 256, 1, 1, 0);
    output[12] = ActivateActCtx(NULL, &cookie); output[13] = cookie != 0;
    one(id++, 4, NULL, 0, 1, 256, 1, 1, 0);
    one(id++, 4, NULL, 0, 2, 256, 1, 1, 0);
    output[14] = DeactivateActCtx(0, cookie); output[15] = id;
    file = CreateFileW(L"result.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, output, used * 4, &written, NULL)
        || written != used * 4) ExitProcess(3);
    CloseHandle(file);
    ExitProcess(0);
}
