/* Authored x86 startup-order and context-preservation oracle.
 * Build twice with CHILD=1 or without it, using test-initial-eip-startup.ps1.
 * Input: an adjacent owned child, normally started or with its initial EIP
 * redirected while suspended. Output: bounded scalar DWORD observations only.
 * The TLS callback deliberately changes volatile GP, flags and FP state.
 */
#include <windows.h>

#define MARKER 0x13572468u
#define FLAGS_MASK 0x8d5u
#define FLAGS_VALUE 0x895u
#define REQUEST_CW 0x0b7fu
#define REQUEST_MXCSR 0x3f80u
static const DWORD request_gp[7] = {
    0x11223344, 0x22334455, 0x33445566, 0x44556677,
    0x55667788, 0x66778899, 0x778899aa
};
static const DWORD request_xmm[4] = {
    0x11112222, 0x33334444, 0x55556666, 0x77778888
};

#ifdef CHILD
__declspec(dllexport) volatile DWORD tls_marker;
__declspec(dllexport) volatile DWORD expected_esp;
static volatile DWORD tls_count;
static DWORD captured_gp[9]; /* EAX, EBX, ECX, EDX, ESI, EDI, EBP, ESP, flags. */
__declspec(align(16)) static BYTE captured_fx[512];
static WORD callback_cw = 0x077f, after_cw;
static DWORD callback_mxcsr = 0x5f80, after_mxcsr;
static const DWORD callback_xmm[4] = {
    0xaaaabbbb, 0xccccdddd, 0xeeeeffff, 0x99990000
};
static DWORD after_xmm[4];

/* A valid callback may clobber volatile registers and arithmetic flags. Record
 * the explicit FP disturbance as well, so a passing preservation check cannot
 * merely reflect a callback which never ran or left the sentinels untouched. */
__declspec(naked) static void WINAPI on_tls(void *module, DWORD reason, void *reserved) {
    __asm {
        cmp dword ptr [esp + 8], 1
        jne finished
        mov dword ptr [tls_marker], MARKER
        inc dword ptr [tls_count]
        fldcw word ptr [callback_cw]
        ldmxcsr dword ptr [callback_mxcsr]
        movdqu xmm0, xmmword ptr [callback_xmm]
        fnstcw word ptr [after_cw]
        stmxcsr dword ptr [after_mxcsr]
        movdqu xmmword ptr [after_xmm], xmm0
        mov ecx, 0xdead1234
        mov edx, 0xbeef5678
        xor eax, eax
    finished:
        ret 12
    }
}

#pragma section(".tls", long, read, write)
#pragma section(".tls$ZZZ", long, read, write)
#pragma section(".CRT$XLA", long, read)
#pragma section(".CRT$XLB", long, read)
#pragma section(".CRT$XLZ", long, read)
#pragma section(".rdata$T", long, read)
__declspec(allocate(".tls")) char tls_start;
__declspec(allocate(".tls$ZZZ")) char tls_end;
DWORD _tls_index;
__declspec(allocate(".CRT$XLA")) PIMAGE_TLS_CALLBACK tls_first = NULL;
__declspec(allocate(".CRT$XLB")) PIMAGE_TLS_CALLBACK tls_callback = on_tls;
__declspec(allocate(".CRT$XLZ")) PIMAGE_TLS_CALLBACK tls_last = NULL;
__declspec(allocate(".rdata$T")) const IMAGE_TLS_DIRECTORY32 _tls_used = {
    (DWORD)&tls_start, (DWORD)&tls_end, (DWORD)&_tls_index,
    (DWORD)(&tls_first + 1), 0, 0
};
#pragma comment(linker, "/include:__tls_used")
#pragma comment(linker, "/include:_tls_callback")

static __declspec(noreturn) void report(DWORD path) {
    DWORD words[20], index, written, mask = 0;
    HANDLE file;
    for (index = 0; index < 7; ++index)
        if (captured_gp[index] == request_gp[index]) mask |= 1u << index;
    words[0] = 0x49504943; words[1] = 2;
    words[2] = tls_marker; words[3] = path; words[4] = tls_count;
    words[5] = mask; words[6] = captured_gp[7] == expected_esp;
    words[7] = captured_gp[8] & FLAGS_MASK;
    words[8] = *(WORD *)(captured_fx + 0);
    words[9] = *(DWORD *)(captured_fx + 24);
    for (index = 0; index < 4; ++index)
        words[10 + index] = *(DWORD *)(captured_fx + 160 + index * 4);
    words[14] = after_cw; words[15] = after_mxcsr;
    for (index = 0; index < 4; ++index) words[16 + index] = after_xmm[index];
    file = CreateFileW(path == 2 ? L"redirect-child.bin" : L"normal-child.bin",
        GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(70);
    if (!WriteFile(file, words, sizeof(words), &written, NULL)
        || written != sizeof(words)) { CloseHandle(file); ExitProcess(71); }
    CloseHandle(file);
    ExitProcess(tls_marker == MARKER ? 37 : 38);
}

/* Capture before any compiler prologue or report call. PUSHAD records the ESP
 * after PUSHFD; adding four reconstructs the actual initial continuation ESP. */
#define CAPTURE_INITIAL \
    __asm { pushfd } \
    __asm { pushad } \
    __asm { mov eax, [esp + 28] } __asm { mov [captured_gp + 0], eax } \
    __asm { mov eax, [esp + 16] } __asm { mov [captured_gp + 4], eax } \
    __asm { mov eax, [esp + 24] } __asm { mov [captured_gp + 8], eax } \
    __asm { mov eax, [esp + 20] } __asm { mov [captured_gp + 12], eax } \
    __asm { mov eax, [esp + 4] } __asm { mov [captured_gp + 16], eax } \
    __asm { mov eax, [esp + 0] } __asm { mov [captured_gp + 20], eax } \
    __asm { mov eax, [esp + 8] } __asm { mov [captured_gp + 24], eax } \
    __asm { mov eax, [esp + 12] } __asm { add eax, 4 } \
    __asm { mov [captured_gp + 28], eax } \
    __asm { mov eax, [esp + 32] } __asm { mov [captured_gp + 32], eax } \
    __asm { fxsave captured_fx } \
    __asm { popad } \
    __asm { popfd }

__declspec(dllexport) __declspec(naked) void redirect(void) {
    CAPTURE_INITIAL
    __asm { push 2 }
    __asm { call report }
    __asm { int 3 }
}
__declspec(naked) void mainCRTStartup(void) {
    CAPTURE_INITIAL
    __asm { push 1 }
    __asm { call report }
    __asm { int 3 }
}
#else
#include "initial-eip-layout.h"
typedef LONG (WINAPI *NT_SET_CONTEXT)(HANDLE, const CONTEXT *);
static DWORD words[46];
static CONTEXT context, checked;
static NT_SET_CONTEXT native_set;

static DWORD gp_mask(const CONTEXT *value) {
    DWORD actual[7], index, mask = 0;
    actual[0] = value->Eax; actual[1] = value->Ebx;
    actual[2] = value->Ecx; actual[3] = value->Edx;
    actual[4] = value->Esi; actual[5] = value->Edi; actual[6] = value->Ebp;
    for (index = 0; index < 7; ++index)
        if (actual[index] == request_gp[index]) mask |= 1u << index;
    return mask;
}
static void child(DWORD at, BOOL redirected) {
    STARTUPINFOW startup;
    PROCESS_INFORMATION info;
    DWORD index, before = 0xaaaaaaaa, transferred = 0, code = 0xaaaaaaaa, initial_esp;
    BYTE *zero = (BYTE *)&startup;
    for (index = 0; index < sizeof(startup); ++index) zero[index] = 0;
    zero = (BYTE *)&info;
    for (index = 0; index < sizeof(info); ++index) zero[index] = 0;
    startup.cb = sizeof(startup);
    words[at] = CreateProcessW(L"initial-eip-child.exe", NULL, NULL, NULL, FALSE,
        CREATE_SUSPENDED | CREATE_NO_WINDOW, NULL, NULL, &startup, &info);
    if (!words[at]) { words[at + 1] = GetLastError(); return; }
    words[at + 1] = ReadProcessMemory(info.hProcess,
        (void *)(CHILD_BASE + CHILD_MARKER_RVA), &before, sizeof(before), &transferred);
    words[at + 2] = before; words[at + 3] = transferred;
    context.ContextFlags = CONTEXT_CONTROL | CONTEXT_INTEGER
        | CONTEXT_FLOATING_POINT | CONTEXT_EXTENDED_REGISTERS;
    words[at + 4] = GetThreadContext(info.hThread, &context);
    words[at + 5] = redirected;
    if (!words[at + 4]) goto cleanup;
    if (redirected) {
        initial_esp = context.Esp;
        context.Eip = CHILD_BASE + CHILD_REDIRECT_RVA;
        context.Eax = request_gp[0]; context.Ebx = request_gp[1];
        context.Ecx = request_gp[2]; context.Edx = request_gp[3];
        context.Esi = request_gp[4]; context.Edi = request_gp[5];
        context.Ebp = request_gp[6];
        context.EFlags = (context.EFlags & ~(FLAGS_MASK | 0x500u)) | FLAGS_VALUE;
        context.FloatSave.ControlWord = REQUEST_CW;
        *(WORD *)(context.ExtendedRegisters + 0) = REQUEST_CW;
        *(DWORD *)(context.ExtendedRegisters + 24) = REQUEST_MXCSR;
        for (index = 0; index < 4; ++index)
            *(DWORD *)(context.ExtendedRegisters + 160 + index * 4) = request_xmm[index];
        words[44] = WriteProcessMemory(info.hProcess,
            (void *)(CHILD_BASE + CHILD_ESP_RVA), &initial_esp, sizeof(initial_esp), &transferred);
        words[45] = transferred;
        if (!words[44] || transferred != sizeof(initial_esp)) goto cleanup;
        words[at + 6] = (DWORD)native_set(info.hThread, &context);
        checked.ContextFlags = context.ContextFlags;
        words[at + 7] = GetThreadContext(info.hThread, &checked)
            && checked.Eip == context.Eip;
        words[32] = checked.ContextFlags; words[33] = context.ContextFlags;
        words[34] = gp_mask(&checked); words[35] = checked.EFlags & FLAGS_MASK;
        words[36] = *(WORD *)(checked.ExtendedRegisters + 0);
        words[37] = *(DWORD *)(checked.ExtendedRegisters + 24);
        for (index = 0; index < 4; ++index)
            words[38 + index] = *(DWORD *)(checked.ExtendedRegisters + 160 + index * 4);
        words[42] = checked.Esp == initial_esp;
        words[43] = checked.FloatSave.ControlWord & 0xffffu;
        if (words[at + 6] || !words[at + 7]) goto cleanup;
    } else { words[at + 6] = 0; words[at + 7] = 1; }
    words[at + 8] = ResumeThread(info.hThread);
    words[at + 9] = WaitForSingleObject(info.hProcess, 5000);
    words[at + 10] = GetExitCodeProcess(info.hProcess, &code);
    words[at + 11] = code;
cleanup:
    if (WaitForSingleObject(info.hProcess, 0) != WAIT_OBJECT_0) {
        words[at + 14] = TerminateProcess(info.hProcess, 72);
        WaitForSingleObject(info.hProcess, 1000);
    }
    words[at + 12] = CloseHandle(info.hThread);
    words[at + 13] = CloseHandle(info.hProcess);
}
void mainCRTStartup(void) {
    HANDLE file; DWORD written;
    native_set = (NT_SET_CONTEXT)GetProcAddress(GetModuleHandleW(L"ntdll.dll"),
        "NtSetContextThread");
    if (!native_set) ExitProcess(73);
    words[0] = 0x49504950; words[1] = 2;
    child(2, FALSE); child(17, TRUE);
    file = CreateFileW(L"initial-eip-parent.bin", GENERIC_WRITE, 0, NULL,
        CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE) ExitProcess(74);
    if (!WriteFile(file, words, sizeof(words), &written, NULL)
        || written != sizeof(words)) { CloseHandle(file); ExitProcess(75); }
    CloseHandle(file); ExitProcess(0);
}
#endif
