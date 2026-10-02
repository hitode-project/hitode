/* Real MinHook and guest memory patching, with deterministic native/PAL output. */
#include "patch_headers/windows.h"
#include "vendor/minhook/include/MinHook.h"

typedef DWORD (WINAPI *PID_FN)(void);
typedef HMODULE (WINAPI *MODULE_FN)(LPCSTR);
typedef DWORD (WINAPI *TARGET_FN)(void);
typedef LONG (WINAPI *QUERY_FN)(LPVOID, LPCVOID, LPVOID *);
typedef DWORD (WINAPI *RELEASE_FN)(LPVOID);
API LONG WINAPI DirectDrawCreateEx(LPVOID, LPVOID *, LPCVOID, LPVOID);
static PID_FN original_pid;
static MODULE_FN original_module;
static QUERY_FN original_query;
static DWORD iat_calls, inline_calls, com_calls;
static HANDLE log_file;
static DWORD check_number;

static void check(BOOL ok) {
    ++check_number;
    if (!ok) ExitProcess(check_number);
}
static void record(const char *line) {
    DWORD n = 0, written;
    while (line[n]) ++n;
    check(WriteFile(log_file, line, n, &written, NULL) && written == n);
}
static BOOL same(const char *a, const char *b) {
    while (*a && *a == *b) { ++a; ++b; }
    return *a == *b;
}
static DWORD WINAPI pid_hook(void) {
    ++iat_calls;
    return original_pid() ? 41 : 0;
}
static HMODULE WINAPI module_hook(LPCSTR name) {
    ++inline_calls;
    return original_module(name);
}
static LONG WINAPI query_hook(LPVOID self, LPCVOID iid, LPVOID *out) {
    ++com_calls;
    return original_query(self, iid, out);
}
static const BYTE iid_ddraw7[16] = {
    0xc0, 0x5e, 0xe6, 0x15, 0x9c, 0x3b, 0xd2, 0x11, 0xb9, 0x2f, 0, 0x60, 0x97, 0x97, 0xea, 0x5b };

/* Find the real imported slot by parsing the executable's PE import descriptors. */
static DWORD *iat_slot(HMODULE module, const char *wanted) {
    BYTE *base = module;
    DWORD pe = *(DWORD *)(base + 0x3c);
    DWORD import_rva = *(DWORD *)(base + pe + 24 + 104);
    DWORD *descriptor = (DWORD *)(base + import_rva);
    for (; descriptor[3]; descriptor += 5) {
        DWORD *names = (DWORD *)(base + descriptor[0]);
        DWORD *slots = (DWORD *)(base + descriptor[4]);
        for (; *names; ++names, ++slots) {
            if (!(*names & 0x80000000) && same((char *)(base + *names + 2), wanted)) return slots;
        }
    }
    return NULL;
}

void mainCRTStartup(void) {
    HMODULE exe = GetModuleHandleA(NULL), kernel = GetModuleHandleA("kernel32.dll");
    DWORD *slot = iat_slot(exe, "GetCurrentProcessId");
    MODULE_FN target = (MODULE_FN)GetProcAddress(kernel, "GetModuleHandleA");
    TARGET_FN patch_target = (TARGET_FN)GetProcAddress(exe, "PatchTarget");
    DWORD previous, ignored, i;
    MEMORY_BASIC_INFORMATION memory;
    BYTE original_entry[16];
    LPVOID draw = NULL, queried = NULL;
    LPVOID *vtable;
    log_file = CreateFileA("patch-compat.log", 0x40000000, 0, NULL, 2, 0x80, NULL);
    check(log_file != INVALID_HANDLE_VALUE && slot && target && patch_target);
    check(VirtualQuery(target, &memory, sizeof(memory)) == sizeof(memory));
    check(memory.State == MEM_COMMIT && (memory.Protect & 0xf0));

    original_pid = (PID_FN)*slot;
    check(VirtualProtect(slot, 4, PAGE_READWRITE, &previous));
    check(VirtualQuery(slot, &memory, sizeof(memory)) == sizeof(memory));
    check(memory.Protect == PAGE_READWRITE);
    *slot = (DWORD)pid_hook;
    check(VirtualProtect(slot, 4, previous, &ignored));
    check(VirtualQuery(slot, &memory, sizeof(memory)) == sizeof(memory));
    check(memory.Protect == previous);
    check(GetCurrentProcessId() == 41 && iat_calls == 1);
    record("iat=41\n");
    check(VirtualProtect(slot, 4, PAGE_READWRITE, &previous));
    *slot = (DWORD)original_pid;
    check(VirtualProtect(slot, 4, previous, &ignored));
    check(GetCurrentProcessId() != 0 && iat_calls == 1);

    for (i = 0; i < 16; ++i) original_entry[i] = ((BYTE *)target)[i];
    check(MH_Initialize() == MH_OK);
    check(MH_CreateHook(target, module_hook, (LPVOID *)&original_module) == MH_OK);
    check(MH_EnableHook(target) == MH_OK);
    /* The first byte must actually be patched. Returning from the detour requires
       the genuine upstream trampoline and a balanced one-argument stdcall frame. */
    check(((BYTE *)target)[0] == 0xe9 || ((BYTE *)target)[0] == 0xeb);
    for (i = 0; i < 16; ++i) check(target(NULL) == exe);
    check(inline_calls == 16 && original_module("kernel32.dll") == kernel);
    record("minhook=16,trampoline=original,stdcall=balanced\n");
    check(MH_DisableHook(target) == MH_OK);
    for (i = 0; i < 16; ++i) check(original_entry[i] == ((BYTE *)target)[i]);
    check(target(NULL) == exe && inline_calls == 16);
    check(MH_RemoveHook(target) == MH_OK && MH_Uninitialize() == MH_OK);
    record("minhook-restored=1\n");

    check(DirectDrawCreateEx(NULL, &draw, iid_ddraw7, NULL) == 0 && draw != NULL);
    vtable = *(LPVOID **)draw;
    original_query = (QUERY_FN)vtable[0];
    check(VirtualProtect(vtable, 4, PAGE_READWRITE, &previous));
    vtable[0] = query_hook;
    check(VirtualProtect(vtable, 4, previous, &ignored));
    check(((QUERY_FN)(*(LPVOID **)draw)[0])(draw, iid_ddraw7, &queried) == 0);
    check(queried != NULL && com_calls == 1);
    record("com-vtable=QueryInterface,original=1\n");
    check(VirtualProtect(vtable, 4, PAGE_READWRITE, &previous));
    vtable[0] = (LPVOID)original_query;
    check(VirtualProtect(vtable, 4, previous, &ignored));
    ((RELEASE_FN)(*(LPVOID **)queried)[2])(queried);
    ((RELEASE_FN)(*(LPVOID **)draw)[2])(draw);

    for (i = 0; i < 128; ++i) check(patch_target() == 17);
    check(LoadLibraryA("fixture_patch_dllmain.dll") != NULL);
    for (i = 0; i < 16; ++i) check(patch_target() == 33);
    record("dllmain-code=17->33\n");
    check(CloseHandle(log_file));
    ExitProcess(0);
}
