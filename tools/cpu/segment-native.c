/* PUSH and POP of segment registers on current WoW64. Run by
   test-segment-native.ps1, which compares the printed results. */
typedef unsigned long DWORD;
typedef void *HANDLE;
#define API __declspec(dllimport)
#define WINAPI __stdcall
API HANDLE WINAPI GetStdHandle(DWORD);
API int WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
API void WINAPI ExitProcess(DWORD);
API void *WINAPI AddVectoredExceptionHandler(DWORD, long (WINAPI *)(void *));

static char out[4096];
static DWORD used;
static void put(const char *s) { while (*s) out[used++] = *s++; }
static void hex(DWORD v) {
    put("0x");
    for (int i = 28; i >= 0; i -= 4) out[used++] = "0123456789abcdef"[(v >> i) & 15];
}
static void line(const char *name, DWORD a, DWORD b) { put(name); put(" "); hex(a); put(" "); hex(b); put("\r\n"); }

static volatile DWORD fault_code, resume_at, info[3];
static long WINAPI handler(void *opaque) {
    DWORD **pointers = (DWORD **)opaque;
    unsigned char *context = (unsigned char *)pointers[1];
    fault_code = pointers[0][0];
    info[0] = pointers[0][4]; info[1] = pointers[0][5]; info[2] = pointers[0][6];
    *(DWORD *)(context + 0xb8) = resume_at; /* Eip */
    *(DWORD *)(context + 0xc4) += 4;        /* discard the pushed value */
    return -1;                               /* EXCEPTION_CONTINUE_EXECUTION */
}

/* 32-bit PUSH: slot pre-filled with 0xdeadbeef; returns slot and ESP delta. */
#define PUSH32(name, bytes) do { DWORD slot, delta; \
    __asm__ volatile("movl %%esp, %%edx\n\tpushl $0xdeadbeef\n\tpopl %%eax\n\t" \
        ".byte " bytes "\n\tmovl (%%esp), %%eax\n\tsubl %%esp, %%edx\n\taddl $4, %%esp" \
        : "=a"(slot), "=d"(delta) :: "memory"); line(name, slot, delta); } while (0)
/* 16-bit PUSH. */
#define PUSH16(name, bytes) do { DWORD slot, delta; \
    __asm__ volatile("movl %%esp, %%edx\n\tpushl $0xdeadbeef\n\tpopl %%eax\n\t" \
        ".byte 0x66," bytes "\n\tmovl -2(%%esp), %%eax\n\tmovl %%esp, %%ecx\n\tsubl %%ecx, %%edx\n\taddl $2, %%esp" \
        : "=a"(slot), "=d"(delta) :: "ecx", "memory"); line(name, slot, delta); } while (0)

/* POP into a register, read it back, restore; faults resume at 1f. */
#define POP(name, pop, read, restore, value) do { DWORD got = 0xffffffff; fault_code = 0; \
    __asm__ volatile("movl $1f, %%eax\n\tmovl %%eax, %1\n\tpushl %2\n\t.byte " pop "\n\t" \
        read "\n\t" restore "\n1:" \
        : "+&d"(got), "=m"(resume_at) : "r"((DWORD)(value)) : "eax", "memory"); \
    line(name, got, fault_code); } while (0)

/* MOV Sreg, ECX with the same stack shape, so a fault resumes alike. */
#define MOV(name, mov, read, restore, value) do { DWORD got = 0xffffffff; fault_code = 0; \
    __asm__ volatile("movl $1f, %%eax\n\tmovl %%eax, %1\n\tpushl %2\n\tmovl %2, %%ecx\n\t" \
        ".byte " mov "\n\taddl $4, %%esp\n\t" read "\n\t" restore "\n1:" \
        : "+&d"(got), "=m"(resume_at) : "r"((DWORD)(value)) : "eax", "ecx", "memory"); \
    line(name, got, fault_code); } while (0)

#define READ_ES "movl $0, %%edx\n\tmovw %%es, %%dx"
#define READ_DS "movl $0, %%edx\n\tmovw %%ds, %%dx"
#define READ_FS "movl $0, %%edx\n\tmovw %%fs, %%dx"
#define READ_GS "movl $0, %%edx\n\tmovw %%gs, %%dx"
#define READ_SS "movl $0, %%edx\n\tmovw %%ss, %%dx"
#define FIX_ES "pushl $0x2b\n\tpopl %%es"
#define FIX_DS "pushl $0x2b\n\tpopl %%ds"
#define FIX_FS "pushl $0x53\n\tpopl %%fs"
#define FIX_SS "pushl $0x2b\n\tpopl %%ss"

void mainCRTStartup(void) {
    DWORD s[6];
    __asm__ volatile("movl $0,%%eax\n\tmovw %%es,%%ax\n\tmovl %%eax,0(%0)\n\t"
        "movl $0,%%eax\n\tmovw %%cs,%%ax\n\tmovl %%eax,4(%0)\n\t"
        "movl $0,%%eax\n\tmovw %%ss,%%ax\n\tmovl %%eax,8(%0)\n\t"
        "movl $0,%%eax\n\tmovw %%ds,%%ax\n\tmovl %%eax,12(%0)\n\t"
        "movl $0,%%eax\n\tmovw %%fs,%%ax\n\tmovl %%eax,16(%0)\n\t"
        "movl $0,%%eax\n\tmovw %%gs,%%ax\n\tmovl %%eax,20(%0)" :: "r"(s) : "eax", "memory");
    line("sel es cs", s[0], s[1]); line("sel ss ds", s[2], s[3]); line("sel fs gs", s[4], s[5]);
    PUSH32("push32 es", "0x06"); PUSH32("push32 cs", "0x0e"); PUSH32("push32 ss", "0x16");
    PUSH32("push32 ds", "0x1e"); PUSH32("push32 fs", "0x0f,0xa0"); PUSH32("push32 gs", "0x0f,0xa8");
    PUSH16("push16 es", "0x06"); PUSH16("push16 fs", "0x0f,0xa0");
    if (!AddVectoredExceptionHandler(1, handler)) ExitProcess(1);
    POP("pop es 2b", "0x07", READ_ES, FIX_ES, 0x2b);
    POP("pop es 28", "0x07", READ_ES, FIX_ES, 0x28);
    POP("pop es 23", "0x07", READ_ES, FIX_ES, 0x23);
    POP("pop es 53", "0x07", READ_ES, FIX_ES, 0x53);
    POP("pop es 30", "0x07", READ_ES, FIX_ES, 0x30);
    POP("pop es 00", "0x07", READ_ES, FIX_ES, 0);
    POP("pop es 03", "0x07", READ_ES, FIX_ES, 3);
    POP("pop es 1234", "0x07", READ_ES, FIX_ES, 0x1234);
    POP("pop es hi2b", "0x07", READ_ES, FIX_ES, 0xabcd002b);
    POP("pop ds 23", "0x1f", READ_DS, FIX_DS, 0x23);
    POP("pop ds 00", "0x1f", READ_DS, FIX_DS, 0);
    POP("pop gs 00", "0x0f,0xa9", READ_GS, "", 0);
    POP("pop gs 2b", "0x0f,0xa9", READ_GS, "", 0x2b);
    POP("pop fs 53", "0x0f,0xa1", READ_FS, FIX_FS, 0x53);
    line("gp info count", info[0], 0); line("gp info", info[1], info[2]);
    POP("pop ss 2b", "0x17", READ_SS, FIX_SS, 0x2b);
    POP("pop ss 00", "0x17", READ_SS, FIX_SS, 0);
    POP("pop ss 23", "0x17", READ_SS, FIX_SS, 0x23);
    /* MOV Sreg, r32 uses the low word; the same checks as POP apply. */
    MOV("mov es 2b", "0x8e,0xc1", READ_ES, FIX_ES, 0x2b);
    MOV("mov es hi23", "0x8e,0xc1", READ_ES, FIX_ES, 0xabcd0023);
    MOV("mov es 1234", "0x8e,0xc1", READ_ES, FIX_ES, 0x1234);
    MOV("mov ss 00", "0x8e,0xd1", READ_SS, FIX_SS, 0);
    MOV("mov ds 00", "0x8e,0xd9", READ_DS, FIX_DS, 0);
    DWORD written;
    WriteFile(GetStdHandle((DWORD)-11), out, used, &written, 0);
    ExitProcess(0);
}
