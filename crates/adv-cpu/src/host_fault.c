/* Windows x64 fault boundary. Guest recovery changes only the interrupted JIT
 * CONTEXT RIP; it does not longjmp, unwind, or skip an intervening Rust frame. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stdint.h>

typedef uintptr_t (*adv_fault_dispatch_t)(uint32_t, uintptr_t, uintptr_t,
                                        uintptr_t, void *);
static adv_fault_dispatch_t adv_dispatch;

static LONG CALLBACK adv_veh(EXCEPTION_POINTERS *exception) {
    EXCEPTION_RECORD *record = exception->ExceptionRecord;
    CONTEXT *context = exception->ContextRecord;
    if (record->ExceptionCode != EXCEPTION_ACCESS_VIOLATION ||
        record->NumberParameters < 2 || !adv_dispatch) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    uintptr_t recovery = adv_dispatch(record->ExceptionCode, context->Rip,
        record->ExceptionInformation[1], record->ExceptionInformation[0], context);
    if (!recovery) return EXCEPTION_CONTINUE_SEARCH;
    context->Rip = recovery;
    return EXCEPTION_CONTINUE_EXECUTION;
}

void *adv_host_fault_install(adv_fault_dispatch_t dispatch) {
    /* The Rust OnceLock publishes this immutable callback before any run. */
    adv_dispatch = dispatch;
    return AddVectoredExceptionHandler(1, adv_veh);
}

/* The OS needs unwind metadata for generated non-leaf frames even when our VEH
 * deliberately refuses a fault. All three RVAs must fit the same 4GB window.
 * Metadata is private read-only memory and is retired before executable code. */
typedef struct {
    RUNTIME_FUNCTION function;
    void *metadata;
} adv_unwind_registration_t;

void *adv_host_unwind_register(uintptr_t code, size_t code_size,
                              const void *metadata, size_t metadata_size) {
    if (!code_size || !metadata_size || code_size > UINT32_MAX ||
        metadata_size > UINT32_MAX || code > UINTPTR_MAX - code_size) {
        SetLastError(ERROR_INVALID_PARAMETER);
        return NULL;
    }
    SYSTEM_INFO system;
    GetSystemInfo(&system);
    uintptr_t end = code + code_size;
    /* Keep the dynamic table's base at the function itself. Allowing xdata
     * below code makes Base..End span nearly 4GB and overlap unrelated dynamic
     * tables; Windows may select that table before searching the right one. */
    uintptr_t lowest = end;
    uintptr_t highest = code > UINTPTR_MAX - UINT32_MAX ? UINTPTR_MAX : code + UINT32_MAX;
    if (lowest < (uintptr_t)system.lpMinimumApplicationAddress)
        lowest = (uintptr_t)system.lpMinimumApplicationAddress;
    if (highest > (uintptr_t)system.lpMaximumApplicationAddress)
        highest = (uintptr_t)system.lpMaximumApplicationAddress;
    uintptr_t granularity = system.dwAllocationGranularity;
    lowest = (lowest + granularity - 1) & ~(granularity - 1);
    highest = ((highest + 1) & ~(granularity - 1)) - 1;
    if (lowest >= highest || highest - lowest < metadata_size) {
        SetLastError(ERROR_INVALID_ADDRESS);
        return NULL;
    }
    MEM_ADDRESS_REQUIREMENTS requirements = { (void *)lowest, (void *)highest, 0 };
    MEM_EXTENDED_PARAMETER parameter = { 0 };
    parameter.Type = MemExtendedParameterAddressRequirements;
    parameter.Pointer = &requirements;
    void *storage = VirtualAlloc2(NULL, NULL, metadata_size, MEM_COMMIT | MEM_RESERVE,
                                 PAGE_READWRITE, &parameter, 1);
    if (!storage) return NULL;
    /* UnwindData's low bits encode indirection, so its RVA, not only its
     * absolute address, must be DWORD aligned. Tail functions may start at
     * an unaligned byte offset inside the JIT allocation. */
    uintptr_t base = code & ~(uintptr_t)3;
    if (end - base > UINT32_MAX || (uintptr_t)storage < end || (uintptr_t)storage - base > UINT32_MAX) {
        VirtualFree(storage, 0, MEM_RELEASE);
        SetLastError(ERROR_INVALID_ADDRESS);
        return NULL;
    }
    CopyMemory(storage, metadata, metadata_size);
    DWORD old;
    if (!VirtualProtect(storage, metadata_size, PAGE_READONLY, &old)) {
        DWORD error = GetLastError();
        VirtualFree(storage, 0, MEM_RELEASE);
        SetLastError(error);
        return NULL;
    }
    adv_unwind_registration_t *registration = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY,
                                                        sizeof(*registration));
    if (!registration) {
        VirtualFree(storage, 0, MEM_RELEASE);
        SetLastError(ERROR_NOT_ENOUGH_MEMORY);
        return NULL;
    }
    registration->metadata = storage;
    registration->function.BeginAddress = (DWORD)(code - base);
    registration->function.EndAddress = (DWORD)(end - base);
    registration->function.UnwindData = (DWORD)((uintptr_t)storage - base);
    if (!RtlAddFunctionTable(&registration->function, 1, base)) {
        VirtualFree(storage, 0, MEM_RELEASE);
        HeapFree(GetProcessHeap(), 0, registration);
        SetLastError(ERROR_INVALID_DATA);
        return NULL;
    }
    return registration;
}

int adv_host_unwind_unregister(void *raw) {
    adv_unwind_registration_t *registration = raw;
    if (!RtlDeleteFunctionTable(&registration->function)) return 0;
    VirtualFree(registration->metadata, 0, MEM_RELEASE);
    HeapFree(GetProcessHeap(), 0, registration);
    return 1;
}

int adv_host_unwind_lookup(uintptr_t pc) {
    DWORD64 base;
    return RtlLookupFunctionEntry(pc, &base, NULL) != NULL;
}

int adv_host_unwind_aligned(uintptr_t pc) {
    DWORD64 base;
    PRUNTIME_FUNCTION function = RtlLookupFunctionEntry(pc, &base, NULL);
    return function && !(function->UnwindData & 3);
}

uint64_t adv_host_fault_gpr(const void *raw, unsigned index) {
    const CONTEXT *context = (const CONTEXT *)raw;
    switch (index) {
    case 0: return context->Rax; case 1: return context->Rcx;
    case 2: return context->Rdx; case 3: return context->Rbx;
    case 4: return context->Rsp; case 5: return context->Rbp;
    case 6: return context->Rsi; case 7: return context->Rdi;
    case 8: return context->R8; case 9: return context->R9;
    case 10: return context->R10; case 11: return context->R11;
    case 12: return context->R12; case 13: return context->R13;
    case 14: return context->R14; case 15: return context->R15;
    default: return 0;
    }
}

/* Diagnostic boundary only. An unrecognized fault falls through our VEH and
 * reaches this ordinary C SEH filter; it never unwinds across a Rust frame. */
typedef uint32_t (*adv_fault_thunk_t)(void *, void *);
typedef struct {
    uint32_t result;
    uint32_t exception;
    uintptr_t pc;
    uintptr_t address;
} adv_probe_result_t;

static LONG adv_probe_filter(EXCEPTION_POINTERS *exception, adv_probe_result_t *out) {
    out->exception = exception->ExceptionRecord->ExceptionCode;
    out->pc = (uintptr_t)exception->ExceptionRecord->ExceptionAddress;
    if (exception->ExceptionRecord->NumberParameters >= 2)
        out->address = exception->ExceptionRecord->ExceptionInformation[1];
    return EXCEPTION_EXECUTE_HANDLER;
}

void adv_host_fault_probe(adv_fault_thunk_t thunk, void *state, void *memory,
                          adv_probe_result_t *out) {
    ZeroMemory(out, sizeof(*out));
    __try { out->result = thunk(state, memory); }
    __except(adv_probe_filter(GetExceptionInformation(), out)) {
        out->result = UINT32_MAX;
    }
}
