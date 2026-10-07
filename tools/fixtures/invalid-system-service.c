/* Authored x86 INT 2E oracle with out-of-range service selectors.
 * Output contains only scalar status/exception/register invariants, no addresses.
 */
#include <windows.h>

static DWORD values[64], active, service_number, returned, result;
static DWORD record_code, exception_flags, exception_parameters, exception_count;
static DWORD record_offset, context_offset, before_flags, after_flags;
static DWORD expected_esp, expected_edx, saved_esp, saved_ebx, saved_ecx;
static DWORD saved_edx, saved_esi, saved_edi;
static DWORD arguments[16];
static void *fault_address, *resume_address;

__declspec(naked) static void probe(void) {
    __asm {
        push ebx
        push esi
        push edi
        cmp active, 2
        jge prefix_setup
        lea eax, system_interrupt
        mov fault_address, eax
        lea eax, resumed
        mov resume_address, eax
        jmp register_setup
    prefix_setup:
        lea eax, prefix_interrupt
        mov fault_address, eax
        lea eax, prefix_resumed
        mov resume_address, eax
    register_setup:
        mov ebx, 13572468h
        mov ecx, 24681357h
        lea edx, arguments
        mov expected_edx, edx
        mov esi, 31415926h
        mov edi, 27182818h
        mov expected_esp, esp
        mov eax, service_number
        cmp active, 2
        jge prefix_probe
        pushfd
        pop before_flags
    system_interrupt:
        int 2eh
        mov returned, 1
        mov result, eax
    resumed:
        jmp capture
    prefix_probe:
        pushfd
        pop before_flags
    prefix_interrupt:
        _emit 0x66
        int 2eh
        mov returned, 1
        mov result, eax
    prefix_resumed:
    capture:
        mov saved_esp, esp
        mov saved_ebx, ebx
        mov saved_ecx, ecx
        mov saved_edx, edx
        mov saved_esi, esi
        mov saved_edi, edi
        pushfd
        pop after_flags
        pop edi
        pop esi
        pop ebx
        ret
    }
}

static LONG WINAPI handler(EXCEPTION_POINTERS *event) {
    EXCEPTION_RECORD *record = event->ExceptionRecord;
    CONTEXT *context = event->ContextRecord;
    if (context->Eip < (DWORD)fault_address || context->Eip >= (DWORD)resume_address)
        return EXCEPTION_CONTINUE_SEARCH;
    record_code = record->ExceptionCode;
    exception_flags = record->ExceptionFlags;
    exception_parameters = record->NumberParameters;
    exception_count++;
    record_offset = (DWORD)record->ExceptionAddress - (DWORD)fault_address;
    context_offset = context->Eip - (DWORD)fault_address;
    result = context->Eax;
    context->Eip = (DWORD)resume_address;
    return EXCEPTION_CONTINUE_EXECUTION;
}

void mainCRTStartup(void) {
    static const DWORD selectors[2] = {0xffffffff, 0x00000fff};
    HANDLE file;
    DWORD written, *out;
    PVOID token = AddVectoredExceptionHandler(1, handler);
    if (!token)
        ExitProcess(1);
    for (active = 0; active < 4; active++) {
        service_number = selectors[active % 2];
        returned = result = record_code = exception_flags = exception_parameters = 0;
        exception_count = record_offset = context_offset = 0;
        SetLastError(0x13572468);
        probe();
        out = values + active * 16;
        out[0] = service_number;
        out[1] = returned;
        out[2] = result;
        out[3] = record_code;
        out[4] = exception_flags;
        out[5] = exception_parameters;
        out[6] = record_offset;
        out[7] = context_offset;
        out[8] = exception_count;
        out[9] = before_flags == after_flags;
        out[10] = expected_esp == saved_esp;
        out[11] = saved_ebx == 0x13572468;
        out[12] = saved_ecx == 0x24681357;
        out[13] = saved_edx == expected_edx;
        out[14] = saved_esi == 0x31415926 && saved_edi == 0x27182818;
        out[15] = GetLastError() == 0x13572468;
    }
    if (!RemoveVectoredExceptionHandler(token))
        ExitProcess(3);
    file = CreateFileW(L"invalid-system-service.bin", GENERIC_WRITE, 0, NULL,
                       CREATE_ALWAYS, 0, NULL);
    if (file == INVALID_HANDLE_VALUE ||
        !WriteFile(file, values, sizeof(values), &written, NULL) ||
        written != sizeof(values))
        ExitProcess(2);
    CloseHandle(file);
    ExitProcess(0);
}
