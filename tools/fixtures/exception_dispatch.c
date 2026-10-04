/* Authored x86 Windows exception oracle. No CRT, windows, audio or game data. */
#include <windows.h>

static DWORD words[256], used;
static DWORD fault_pc, resume_pc, phase, repaired_seh_head;
static volatile DWORD *page;
static WORD desired_cw = 0x077f, after_cw;
static DWORD desired_mxcsr = 0x5f80, after_mxcsr;
static BYTE vector_before[16] = {1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16};
static BYTE vector_after[16], float_after[10];
static void dump(void) {
    DWORD written;
    HANDLE file = CreateFileA("exception-dispatch.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    if (file != INVALID_HANDLE_VALUE) { WriteFile(file, words, used * 4, &written, NULL); CloseHandle(file); }
}
static void record(DWORD value) { if (used < 256) words[used++] = value; else { dump(); ExitProcess(90); } }
static void fail_if(BOOL condition, DWORD code) { if (condition) ExitProcess(code); }

static void observe(EXCEPTION_RECORD *record_, CONTEXT *context) {
    record(record_->ExceptionCode);
    record(record_->ExceptionFlags);
    record(record_->NumberParameters);
    if (phase == 1) {
        record(record_->ExceptionInformation[0]);
        record(record_->ExceptionInformation[1]);
        HMODULE owner = NULL;
        record(record_->ExceptionAddress == (void *)context->Eip);
        record(GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                                 (LPCSTR)record_->ExceptionAddress, &owner));
        record(owner != NULL && owner != GetModuleHandleA(NULL));
        return;
    }
    record((DWORD)record_->ExceptionAddress == fault_pc);
    record(context->Eip == fault_pc);
    record(context->Eax);
    record(record_->ExceptionInformation[0]);
    record(record_->ExceptionInformation[1] == (DWORD)page);
    if (phase == 4) {
        record((context->ContextFlags & (CONTEXT_FLOATING_POINT | CONTEXT_EXTENDED_REGISTERS)) ==
               (CONTEXT_FLOATING_POINT | CONTEXT_EXTENDED_REGISTERS));
        record(context->FloatSave.ControlWord & 0xffff);
        record(*(DWORD *)(context->ExtendedRegisters + 24));
        record(context->ExtendedRegisters[160 + 16]); // XMM1 byte 0.
        record(context->ExtendedRegisters[160 + 31]); // XMM1 byte 15.
    }
}

static LONG CALLBACK observer(EXCEPTION_POINTERS *p) {
    record(0x100 + phase);
    observe(p->ExceptionRecord, p->ContextRecord);
    Sleep(0); // The runtime must allow a handler to yield on its ordinary scheduler.
    return EXCEPTION_CONTINUE_SEARCH;
}

static LONG CALLBACK resolver(EXCEPTION_POINTERS *p) {
    record(0x200 + phase);
    if (phase == 5) {
        __asm__ volatile("movl %0,%%fs:0" : : "r"(repaired_seh_head) : "memory");
        return EXCEPTION_CONTINUE_SEARCH;
    }
    if (phase != 1) {
        p->ContextRecord->Eip = resume_pc;
        p->ContextRecord->Eax = 0xaabbccdd;
    }
    if (phase == 4) {
        // Clobber live handler FPU/vector state. Continuing must restore the
        // interrupted architectural image, not these callback temporaries.
        __asm__ volatile("fninit\n\tpxor %%xmm1,%%xmm1" : : : "memory", "xmm1");
    }
    if (phase == 6 || phase == 8) p->ContextRecord->FloatSave.ControlWord = 0x0b7f;
    if (phase == 7 || phase == 8) *(WORD *)p->ContextRecord->ExtendedRegisters = 0x0f7f;
    if (phase == 9) {
        ULONG_PTR inner[2] = {0x11223344, 0x55667788};
        phase = 1;
        RaiseException(0xe0420002, 0, 2, inner);
        record(0x402);
        phase = 9;
    }
    return EXCEPTION_CONTINUE_EXECUTION;
}

typedef struct Registration {
    struct Registration *previous;
    LONG (__cdecl *handler)(EXCEPTION_RECORD *, void *, CONTEXT *, void *);
} Registration;
static Registration *expected_frame;

static LONG __cdecl stack_handler(EXCEPTION_RECORD *exception, void *frame, CONTEXT *context, void *dispatcher) {
    (void)dispatcher;
    record(0x300 + phase);
    record(frame == expected_frame);
    record(exception->ExceptionCode);
#ifdef NESTED_FRAME
    RaiseException(0xe0420003, 0, 0, NULL);
    ExitProcess(77); // The unsupported runtime case must stop before re-entering this frame.
#endif
    context->Eip = resume_pc;
    context->Eax = 0xaabbccdd;
    return 0; // ExceptionContinueExecution for a frame-based handler.
}

static void read_fault(void) {
    DWORD result = 0x12345678;
    __asm__ volatile(
        "movl $1f,%1\n\tmovl $2f,%2\n\t1: movl (%%ecx),%%eax\n\t2:"
        : "+a"(result), "=m"(fault_pc), "=m"(resume_pc) : "c"(page) : "memory");
    record(result);
}
static void write_fault(void) {
    DWORD result = 0x12345678;
    __asm__ volatile(
        "movl $1f,%1\n\tmovl $2f,%2\n\t1: movl %%eax,(%%ecx)\n\t2:"
        : "+a"(result), "=m"(fault_pc), "=m"(resume_pc) : "c"(page) : "memory");
    record(result);
}

void mainCRTStartup(void) {
    DWORD protection, written;
    page = (volatile DWORD *)VirtualAlloc(NULL, 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    fail_if(!page, 10);
    *page = 0xdeadbeef;
    fail_if(!VirtualProtect((void *)page, 4096, PAGE_NOACCESS, &protection), 11);
    void *second = AddVectoredExceptionHandler(0, resolver);
    void *first = AddVectoredExceptionHandler(1, observer);
    fail_if(!first || !second, 12);

    phase = 1;
    ULONG_PTR arguments[2] = {0xabcdef01, 0x76543210};
    RaiseException(0xe0420001, 0, 2, arguments);
    record(0x401);
    phase = 2;
    read_fault();
    phase = 3;
    write_fault();
    phase = 4;
    __asm__ volatile("fninit\n\tfldcw %0\n\tldmxcsr %1\n\tfld1\n\tmovdqu %2,%%xmm1"
        : : "m"(desired_cw), "m"(desired_mxcsr), "m"(vector_before) : "memory", "xmm1");
    read_fault();
    __asm__ volatile("fnstcw %0\n\tstmxcsr %1\n\tfstpt %2\n\tmovdqu %%xmm1,%3"
        : "=m"(after_cw), "=m"(after_mxcsr), "=m"(float_after), "=m"(vector_after) : : "memory");
    record(after_cw);
    record(after_mxcsr);
    for (DWORD n = 0; n < 10; ++n) record(float_after[n]);
    for (DWORD n = 0; n < 16; ++n) record(vector_after[n]);
    __asm__ volatile("fninit");

    phase = 5;
    Registration registration;
    registration.handler = stack_handler;
    expected_frame = &registration;
    __asm__ volatile("movl %%fs:0,%0" : "=r"(registration.previous) : : "memory");
    repaired_seh_head = (DWORD)&registration; // VEH publishes it before frame search.
    read_fault();
    __asm__ volatile("movl %0,%%fs:0" : : "r"(registration.previous) : "memory");
    for (phase = 6; phase <= 9; ++phase) {
        __asm__ volatile("fldcw %0" : : "m"(desired_cw));
        read_fault();
        __asm__ volatile("fnstcw %0" : "=m"(after_cw));
        record(after_cw);
    }
    record(RemoveVectoredExceptionHandler(first));
    record(RemoveVectoredExceptionHandler(second));
    fail_if(!VirtualProtect((void *)page, 4096, PAGE_READWRITE, &protection), 13);
    record(*page == 0xdeadbeef);
    record(VirtualFree((void *)page, 0, MEM_RELEASE));
    HANDLE file = CreateFileA("exception-dispatch.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    fail_if(file == INVALID_HANDLE_VALUE, 14);
    fail_if(!WriteFile(file, words, used * 4, &written, NULL) || written != used * 4, 15);
    CloseHandle(file);
    ExitProcess(0);
}
