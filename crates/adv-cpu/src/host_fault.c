/* Windows x64 fault boundary. Guest recovery changes only the interrupted JIT
 * CONTEXT RIP; it does not longjmp, unwind, or skip an intervening Rust frame. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stdint.h>
#include <stdlib.h>

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
    size_t capacity;
} adv_unwind_registration_t;

/* One function owns one tail page. The first DWORD is a duplicate-registration
 * marker; the xdata begins at +16, independently of code's byte alignment. */
#define ADV_XDATA_HEADER 16

static int adv_xdata_release(void *storage, size_t capacity) {
    DWORD old;
    if (!VirtualProtect(storage, capacity, PAGE_READWRITE, &old)) return 0;
    *(volatile LONG *)storage = 0;
    if (!VirtualProtect(storage, capacity, PAGE_READONLY, &old)) return 0;
    return 1;
}

void *adv_host_unwind_register(uintptr_t code, size_t code_size,
                              const void *metadata, size_t metadata_size,
                              void *storage, size_t capacity) {
    uintptr_t base = code & ~(uintptr_t)3;
    uintptr_t xdata = (uintptr_t)storage + ADV_XDATA_HEADER;
    if (!code_size || !metadata_size || !storage || capacity < ADV_XDATA_HEADER ||
        metadata_size > capacity - ADV_XDATA_HEADER || code > UINTPTR_MAX - code_size ||
        code + code_size - base > UINT32_MAX || xdata < code + code_size ||
        xdata - base > UINT32_MAX || (xdata - base) & 3) {
        SetLastError(ERROR_INVALID_PARAMETER);
        return NULL;
    }
    /* Rust checked both code RX and this disjoint RO page against VirtualQuery
     * and the same provider reservation before passing its exclusive slot. */
    adv_unwind_registration_t *registration = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY,
                                                        sizeof(*registration));
    if (!registration) { SetLastError(ERROR_NOT_ENOUGH_MEMORY); return NULL; }
    DWORD old;
    if (!VirtualProtect(storage, capacity, PAGE_READWRITE, &old)) {
        HeapFree(GetProcessHeap(), 0, registration);
        return NULL;
    }
    if (*(volatile LONG *)storage != 0) {
        if (!VirtualProtect(storage, capacity, PAGE_READONLY, &old)) abort();
        HeapFree(GetProcessHeap(), 0, registration);
        SetLastError(ERROR_ALREADY_EXISTS);
        return NULL;
    }
    *(volatile LONG *)storage = 1;
    CopyMemory((void *)xdata, metadata, metadata_size);
    if (!VirtualProtect(storage, capacity, PAGE_READONLY, &old)) abort();
    registration->metadata = storage;
    registration->capacity = capacity;
    registration->function.BeginAddress = (DWORD)(code - base);
    registration->function.EndAddress = (DWORD)(code + code_size - base);
    registration->function.UnwindData = (DWORD)(xdata - base);
    if (!RtlAddFunctionTable(&registration->function, 1, base)) {
        if (!adv_xdata_release(storage, capacity)) abort();
        HeapFree(GetProcessHeap(), 0, registration);
        SetLastError(ERROR_INVALID_DATA);
        return NULL;
    }
    return registration;
}

int adv_host_unwind_unregister(void *raw) {
    adv_unwind_registration_t *registration = raw;
    if (!RtlDeleteFunctionTable(&registration->function)) return 0;
    if (!adv_xdata_release(registration->metadata, registration->capacity)) return 0;
    /* The provider retains both code and xdata until every table is retired. */
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
