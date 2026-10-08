/* Authored x86 RtlRaiseException caller-address and hook-return oracle. */
#include <windows.h>
#include "vendor/minhook/include/MinHook.h"

typedef VOID(WINAPI *RAISE)(PEXCEPTION_RECORD);
static RAISE raise_native, original;
static volatile DWORD expected, active, handlers, hook_entered, hook_returned;
static DWORD values[20];
static __attribute__((used)) EXCEPTION_RECORD record;

static LONG WINAPI handler(PEXCEPTION_POINTERS pointers) {
    PEXCEPTION_RECORD current = pointers->ExceptionRecord;
    DWORD index = 2 + active * 4;
    if (current->ExceptionCode != 0xe0421001) return EXCEPTION_CONTINUE_SEARCH;
    ++handlers;
    values[index] = (DWORD)current->ExceptionAddress == expected;
    values[index + 2] = current->ExceptionFlags == 0 && current->NumberParameters == 2 &&
        current->ExceptionInformation[0] == 0x12345678 &&
        current->ExceptionInformation[1] == 0xabcdef01;
    if (active == 2) values[index + 1] = (DWORD)current->ExceptionAddress == expected;
    return EXCEPTION_CONTINUE_EXECUTION;
}

/* The label is authored source, not a copied native instruction address. */
__declspec(naked) static VOID WINAPI intercepted(PEXCEPTION_RECORD ignored) {
    __asm__("pushl %ebp\n\tmovl %esp, %ebp\n\tincl _hook_entered\n\t"
            "movl $1f, _expected\n\tpushl 8(%ebp)\n\tcall *_original\n"
            "1:\tincl _hook_returned\n\tpopl %ebp\n\tretl $4");
}

void mainCRTStartup(void) {
    HMODULE nt = GetModuleHandleW(L"ntdll.dll");
    PVOID veh;
    HANDLE file;
    DWORD written;
    ULONG_PTR parameters[2] = {0x12345678, 0xabcdef01};
    raise_native = (RAISE)GetProcAddress(nt, "RtlRaiseException");
    values[0] = raise_native != NULL;
    if (!values[0]) ExitProcess(1);
    veh = AddVectoredExceptionHandler(1, handler);
    values[1] = veh != NULL;
    if (!veh) ExitProcess(2);
    record.ExceptionCode = 0xe0421001;
    record.NumberParameters = 2;
    record.ExceptionInformation[0] = parameters[0];
    record.ExceptionInformation[1] = parameters[1];
    record.ExceptionAddress = (PVOID)0x13572468;
    __asm__ volatile("movl $1f, _expected\n\tpushl $_record\n\tcall *_raise_native\n1:\tnop"
                     : : : "eax", "ecx", "edx", "memory");
    values[3] = (DWORD)record.ExceptionAddress == expected;
    values[5] = 1;
    active = 1;
    record.ExceptionAddress = (PVOID)0x24681357;
    __asm__ volatile("movl $1f, _expected\n\tpushl $_record\n\tcall *_raise_native\n1:\tnop"
                     : : : "eax", "ecx", "edx", "memory");
    values[7] = (DWORD)record.ExceptionAddress == expected;
    values[9] = 1;
    values[14] = MH_Initialize() == MH_OK &&
        MH_CreateHook((PVOID)raise_native, intercepted, (PVOID *)&original) == MH_OK &&
        MH_EnableHook((PVOID)raise_native) == MH_OK;
    if (!values[14]) ExitProcess(3);
    active = 2;
    RaiseException(record.ExceptionCode, 0, 2, parameters);
    values[13] = 1;
    values[15] = hook_entered == 1;
    values[16] = hook_returned == 1;
    values[17] = handlers;
    values[18] = RemoveVectoredExceptionHandler(veh) == 1;
    values[19] = MH_DisableHook(MH_ALL_HOOKS) == MH_OK && MH_Uninitialize() == MH_OK;
    file = CreateFileW(L"rtl-raise-address.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, values, sizeof(values), &written, NULL) ||
        written != sizeof(values)) ExitProcess(4);
    CloseHandle(file);
    ExitProcess(0);
}
