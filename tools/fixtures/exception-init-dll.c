/* Authored faults inside DllMain; every continuation belongs to this fixture. */
#include <windows.h>
typedef void (__cdecl *RECORDER)(DWORD);
typedef DWORD (__cdecl *CASE)(void);
typedef struct Registration { struct Registration *previous; void *handler; } Registration;
static RECORDER record;
static DWORD mode, fault_pc, resume_pc, saved_sp, saved_bp;
static DWORD after_sp, after_bp, after_flags, after_eax, handler_flags;
static DWORD nested, handlers;
static volatile DWORD *page;
static LONG __cdecl search(EXCEPTION_RECORD *r, void *f, CONTEXT *c, void *d) {
    (void)f; (void)c; (void)d;
    record((r->ExceptionFlags & 6) ? 0x611 : 0x610);
    record(r->ExceptionFlags & 7);
    record(r->ExceptionCode);
    return 1;
}

static void observe(EXCEPTION_RECORD *r, CONTEXT *c, DWORD kind) {
    record(kind); record(r->ExceptionCode); record(r->ExceptionFlags);
    record(r->ExceptionAddress == (void *)fault_pc);
    record(c->Eip == fault_pc); record(c->Esp == saved_sp); record(c->Ebp == saved_bp);
    record(c->Eax == 0x12345678);
    record(c->EFlags & 0x10ed5); record((handler_flags & 0x400) != 0);
    record(r->NumberParameters);
    if (r->ExceptionCode == EXCEPTION_ACCESS_VIOLATION) {
        record(r->ExceptionInformation[0]); record(r->ExceptionInformation[1] == (DWORD)page);
    }
}
static void repair(CONTEXT *c) {
    c->Eip = resume_pc;
    c->Eax = 0xabcdef01;
    c->EFlags = (c->EFlags & ~0x8d5) | 0x891;
}
static LONG CALLBACK veh_body(EXCEPTION_POINTERS *p) {
    ++handlers;
    if (p->ExceptionRecord->ExceptionCode == 0xe0423141) {
        record(0x110); record(p->ExceptionRecord->ExceptionFlags);
        return EXCEPTION_CONTINUE_EXECUTION;
    }
    observe(p->ExceptionRecord, p->ContextRecord, 0x101);
    if (mode == 1 || mode == 3 || mode == 7) return EXCEPTION_CONTINUE_SEARCH;
    if (mode == 2) { RaiseException(0xe0423141, 0, 0, NULL); record(0x111); }
    repair(p->ContextRecord);
    return EXCEPTION_CONTINUE_EXECUTION;
}
__declspec(naked) static LONG CALLBACK veh(EXCEPTION_POINTERS *p) {
    __asm {
        pushfd
        pop handler_flags
        cld
        jmp veh_body
    }
}
static LONG __cdecl seh_body(EXCEPTION_RECORD *r, void *frame, CONTEXT *c, void *dispatcher) {
    (void)frame; (void)dispatcher;
    ++handlers;
    observe(r, c, 0x201);
    if (mode == 3 && !nested) {
        nested = 1; RaiseException(0xe0423141, 0, 0, NULL); record(0x211);
    }
    repair(c);
    return 0;
}
__declspec(naked) static LONG __cdecl seh(EXCEPTION_RECORD *r, void *f, CONTEXT *c, void *d) {
    __asm {
        pushfd
        pop handler_flags
        cld
        jmp seh_body
    }
}

#define FAULT(OP) __asm__ volatile( \
    "movl %%esp,%0\n\tmovl %%ebp,%1\n\tmovl $1f,%2\n\tmovl $2f,%3\n\t" \
    "pushl $0xed7\n\tpopfl\n\t1: " OP "\n\t2: pushfl\n\tpopl %4\n\tcld\n\t" \
    "movl %%esp,%5\n\tmovl %%ebp,%6\n\tmovl %%eax,%7" \
    : "=m"(saved_sp), "=m"(saved_bp), "=m"(fault_pc), "=m"(resume_pc), \
      "=m"(after_flags), "=m"(after_sp), "=m"(after_bp), "=m"(after_eax) \
    : "a"(0x12345678), "c"(page), "d"(0) : "memory")

BOOL WINAPI DllMain(HMODULE module, DWORD reason, void *reserved) {
    DWORD old; void *vector; Registration registration;
    (void)module; (void)reserved;
    if (reason != DLL_PROCESS_ATTACH) {
        if (reason == DLL_PROCESS_DETACH && mode >= 10) record(0x4100);
        return TRUE;
    }
    record = (RECORDER)GetProcAddress(GetModuleHandleA(NULL), "Record");
    mode = ((CASE)GetProcAddress(GetModuleHandleA(NULL), "Case"))();
    record(0x4000 + mode);
    if (mode >= 10) {
        if (((CASE)GetProcAddress(GetModuleHandleA(NULL), "Attempt"))()) return TRUE;
        if (mode == 12) return FALSE;
        registration.handler = search;
        __asm__ volatile("movl %%fs:0,%0" : "=r"(registration.previous) : : "memory");
        __asm__ volatile("movl %0,%%fs:0" : : "r"(&registration) : "memory");
        if (mode == 11) RaiseException(0xe0423142, EXCEPTION_NONCONTINUABLE, 0, NULL);
        else __asm__ volatile("xorl %%eax,%%eax; movl %%eax,(%%eax)" : : : "eax", "memory");
        __asm__ volatile("movl %0,%%fs:0" : : "r"(registration.previous) : "memory");
        record(0xdead);
        return TRUE;
    }
    page = (volatile DWORD *)VirtualAlloc(NULL, 4096, MEM_RESERVE|MEM_COMMIT, PAGE_READWRITE);
    if (!page) return FALSE;
    *page = 0x76543210;
    if (!VirtualProtect((void *)page, 4096, PAGE_EXECUTE_READ, &old)) return FALSE;
    vector = mode == 5 ? NULL : AddVectoredExceptionHandler(1, veh);
    registration.handler = seh;
    __asm__ volatile("movl %%fs:0,%0" : "=r"(registration.previous) : : "memory");
    if (mode == 1 || mode == 3 || mode == 7) {
        __asm__ volatile("movl %0,%%fs:0" : : "r"(&registration) : "memory");
    }
    if (mode == 4) { FAULT("int3"); }
    else if (mode == 6 || mode == 7) { FAULT("cli"); }
    else if (mode == 8) { FAULT("ud2"); }
    else if (mode == 9) { FAULT("divl %%edx"); }
    else { FAULT("movl %%eax,(%%ecx)"); }
    __asm__ volatile("movl %0,%%fs:0" : : "r"(registration.previous) : "memory");
    record(0x5000); record(after_eax == 0xabcdef01);
    record(after_sp == saved_sp); record(after_bp == saved_bp);
    record(after_flags & 0xed5); record(*page == 0x76543210); record(handlers);
    record(RemoveVectoredExceptionHandler(vector) != 0);
    record(VirtualFree((void *)page, 0, MEM_RELEASE) != 0);
    return TRUE;
}
