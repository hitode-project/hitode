/* Raw operand-size-prefixed MOVZX/MOVSX word->word on current WoW64.
   The instruction bytes are intentional: assembler aliases can hide the form. */
#include "../fixtures/win32_min.h"

static const DWORD values[] = {0, 1, 0x7fff, 0x8000, 0xffff, 0x12348000, 0xfeedcafe, 0xffffffff};
API void *WINAPI AddVectoredExceptionHandler(DWORD, LONG (WINAPI *)(void *));

static LONG WINAPI divide_fault(void *opaque) {
    /* EXCEPTION_POINTERS starts with ExceptionRecord; its first DWORD is code. */
    DWORD code = **(DWORD **)opaque;
    ExitProcess(code == 0xc0000095 ? 0 : code);
    return -1;
}

#define EXECUTE(opcode, modrm, input) \
    __asm__ volatile("pushfl\n\tpushl $0x8d7\n\tpopfl\n\t" \
        ".byte 0x66,0x0f," #opcode "," #modrm "\n\t" \
        "pushfl\n\tpopl %%edx\n\tpopfl" \
        : "+a"(actual), "=d"(flags) : "c"(input) : "memory", "cc")

void mainCRTStartup(void) {
    DWORD count = 0;
    for (DWORD source = 0; source < 8; ++source) {
        for (DWORD destination = 0; destination < 8; ++destination) {
            for (DWORD form = 0; form < 4; ++form) {
                DWORD actual = values[destination], flags, input = values[source];
                switch (form) {
                    case 0: EXECUTE(0xb7, 0xc1, input); break;
                    case 1: EXECUTE(0xbf, 0xc1, input); break;
                    case 2: EXECUTE(0xb7, 0x01, &input); break;
                    default: EXECUTE(0xbf, 0x01, &input); break;
                }
                if (actual != ((values[destination] & 0xffff0000) | (input & 0xffff))) ExitProcess(10 + form);
                if ((flags & 0x8d5) != 0x8d5) ExitProcess(20 + form);
                ++count;
            }
        }
    }
    if (count != 256) ExitProcess(30);
    if (!AddVectoredExceptionHandler(1, divide_fault)) ExitProcess(31);
    /* SST F6.7 #516: AX=-25487, CL=71. Quotient -358 exceeds signed AL.
       Current hardware must raise #DE, unlike the published 386EX vector. */
    DWORD numerator = 0xa1719c71, denominator = 0x2ba20547;
    __asm__ volatile(".byte 0xf6,0xf9" : "+a"(numerator) : "c"(denominator) : "cc");
    ExitProcess(32);
}
