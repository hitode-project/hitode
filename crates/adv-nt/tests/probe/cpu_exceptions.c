/* CPU exceptions as a vectored handler sees them.
 * Every line it prints was measured on Windows 11 x86 (WoW64): BOUND out of
 * range, INT n through each kind of IDT gate. A protector depends on these
 * codes, addresses and saved EIPs, and on an address Windows leaves free.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o cpu_exceptions.exe cpu_exceptions.c */
#include <windows.h>
#include <stdio.h>

volatile DWORD skip_to;
static struct { DWORD code, address, eip, n, p[2]; } seen;
static DWORD bounds[2] = { (DWORD)-4, 7 };

static LONG CALLBACK handler(EXCEPTION_POINTERS *e)
{
    seen.code = e->ExceptionRecord->ExceptionCode;
    seen.address = (DWORD)e->ExceptionRecord->ExceptionAddress;
    seen.eip = e->ContextRecord->Eip;
    seen.n = e->ExceptionRecord->NumberParameters;
    for (DWORD i = 0; i < 2 && i < seen.n; i++) seen.p[i] = e->ExceptionRecord->ExceptionInformation[i];
    e->ContextRecord->Eip = skip_to;
    return EXCEPTION_CONTINUE_EXECUTION;
}

/* Each instruction is two bytes; the handler resumes after it. */
#define TRY(name, bytes)                                                        \
    do {                                                                        \
        DWORD insn, after;                                                      \
        memset(&seen, 0, sizeof seen);                                          \
        __asm__ volatile("lea 4f, %0\n lea 2f, %1\n mov %1, _skip_to\n"         \
                         "4: " bytes "\n2: nop\n"                               \
                         : "=&r"(insn), "=&r"(after) : "a"(100), "b"(bounds)    \
                         : "memory", "ecx", "edx");                             \
        if (!seen.code) {                                                       \
            printf("%s none\n", name);                                          \
            break;                                                              \
        }                                                                       \
        printf("%s %08lx addr=%+ld eip=%+ld n=%lu", name, seen.code,            \
               (long)(seen.address - insn), (long)(seen.eip - insn), seen.n);   \
        for (DWORD i = 0; i < 2 && i < seen.n; i++) printf(" %lx", seen.p[i]);  \
        printf("\n");                                                           \
    } while (0)

int main(void)
{
    MEMORY_BASIC_INFORMATION m;
    VirtualQuery((void *)0x12364574, &m, sizeof m);
    printf("probe %s\n", m.State == MEM_FREE ? "free" : "used");

    AddVectoredExceptionHandler(1, handler);
    bounds[1] = 200;
    TRY("bound_in", ".byte 0x62, 0x03"); /* bound eax, [ebx] */
    bounds[1] = 7;
    TRY("bound_out", ".byte 0x62, 0x03");
    TRY("int1", ".byte 0xcd, 0x01");
    TRY("int3", ".byte 0xcd, 0x03");
    TRY("int4", ".byte 0xcd, 0x04");
    TRY("int20", ".byte 0xcd, 0x20");
    TRY("int2c", ".byte 0xcd, 0x2c");
    TRY("int80", ".byte 0xcd, 0x80");
    return 0;
}
