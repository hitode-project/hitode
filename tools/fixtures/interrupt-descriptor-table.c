/* Authored x86 SIDT width, flags and exact six-byte memory-write oracle.
 * Only scalar invariants are written; the native IDTR address stays in memory.
 */
#include <windows.h>

#pragma pack(push, 1)
static volatile struct {
    WORD leading;
    WORD limit;
    DWORD base;
    WORD trailing;
} table32 = {0xabcd, 0xa5a5, 0xa5a5a5a5, 0xef01},
  table16 = {0xabcd, 0x5a5a, 0x5a5a5a5a, 0xef01},
  repeated = {0xabcd, 0x3c3c, 0x3c3c3c3c, 0xef01};
#pragma pack(pop)
static DWORD values[8], before, after, saved_edx;

__declspec(naked) static void probe(void) {
    __asm {
        pushfd
        pop before
        mov edx, 13572468h
        sidt fword ptr table32[2]
        _emit 0x66
        sidt fword ptr table16[2]
        sidt fword ptr repeated[2]
        mov saved_edx, edx
        pushfd
        pop after
        ret
    }
}

void mainCRTStartup(void) {
    HANDLE file;
    DWORD written;
    SetLastError(0x13572468);
    probe();
    values[0] = table32.leading == 0xabcd && table32.trailing == 0xef01;
    values[1] = table16.leading == 0xabcd && table16.trailing == 0xef01;
    values[2] = repeated.leading == 0xabcd && repeated.trailing == 0xef01;
    values[3] = table32.limit == table16.limit && table32.base == table16.base;
    values[4] = table32.limit == repeated.limit && table32.base == repeated.base;
    values[5] = before == after;
    values[6] = saved_edx == 0x13572468;
    values[7] = GetLastError() == 0x13572468;
    file = CreateFileW(L"interrupt-descriptor-table.bin", GENERIC_WRITE, 0, NULL,
                       CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE ||
        !WriteFile(file, values, sizeof(values), &written, NULL) ||
        written != sizeof(values))
        ExitProcess(2);
    CloseHandle(file);
    ExitProcess(0);
}
