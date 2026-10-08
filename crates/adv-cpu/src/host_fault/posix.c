/* POSIX context boundary. Recovery changes only the interrupted native PC and
 * returns through a registered same-frame epilogue. No longjmp or unwinding. */
#define _GNU_SOURCE
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdatomic.h>
#include <unistd.h>
#ifdef __APPLE__
#include <sys/ucontext.h>
#else
#include <ucontext.h>
#endif

#if (defined(__linux__) && (defined(__x86_64__) || defined(__aarch64__))) || (defined(__APPLE__) && defined(__aarch64__))
/* Linux UAPI flag, absent from some libc signal.h versions. */
#ifndef SA_EXPOSE_TAGBITS
#ifdef __APPLE__
#define SA_EXPOSE_TAGBITS 0
#else
#define SA_EXPOSE_TAGBITS 0x00000800
#endif
#endif
typedef uintptr_t (*adv_posix_dispatch_t)(int, uintptr_t, uintptr_t,
                                        uintptr_t, void *);
static adv_posix_dispatch_t adv_dispatch;
static struct sigaction adv_previous[2];
_Static_assert(ATOMIC_INT_LOCK_FREE == 2, "signal chaining requires lock-free atomics");
static _Atomic unsigned adv_reset[2];

#if defined(__aarch64__) && !defined(__APPLE__)
struct adv_context_header { uint32_t magic, size; };
struct adv_fpsimd_context {
    struct adv_context_header head;
    uint32_t fpsr, fpcr;
    unsigned char vregs[32][16];
};
static const struct adv_fpsimd_context *adv_fpsimd(const ucontext_t *context) {
    const unsigned char *records = context->uc_mcontext.__reserved;
    size_t offset = 0;
    const struct adv_fpsimd_context *fp = NULL;
    while (offset <= sizeof(context->uc_mcontext.__reserved) - 8) {
        const struct adv_context_header *head = (const void *)(records + offset);
        if (!head->magic && !head->size) return fp;
        if (head->size < 16 || head->size % 16 ||
            head->size > sizeof(context->uc_mcontext.__reserved) - offset) return NULL;
        if (head->magic == 0x46508001) {
            if (head->size < sizeof(struct adv_fpsimd_context) || fp) return NULL;
            fp = (const void *)head;
        } else if (head->magic == 0x53564501) {
            /* Linux adds an SVE record on every SVE-capable CPU, header-only
             * when no SVE state is live. Outside streaming mode the kernel
             * still copies each Z register's low 128 bits, FPSR and FPCR into
             * the FPSIMD record, and recovery changes only the PC, so that
             * record stays authoritative. Streaming mode is not admitted. */
            if (head->size < 16 || *(const uint16_t *)(records + offset + 10) & 1) return NULL;
        } else if (head->magic == 0x54504902 || head->magic == 0x54366345) {
            /* SME-capable Linux also supplies TPIDR2 and a header-only ZA
             * record when ZA is disabled. Neither changes the FPSIMD image;
             * sigreturn preserves TPIDR2 unchanged. A ZA payload represents
             * live matrix state, which this snapshot does not admit. */
            if (head->size != 16) return NULL;
        } else if (head->magic != 0x45535201) {
            /* Extension storage and unknown records are not admitted. */
            return NULL;
        }
        offset += head->size;
    }
    return NULL;
}
#endif

uintptr_t adv_posix_fault_pc(const void *raw) {
    const ucontext_t *context = raw;
#if defined(__x86_64__)
    return context->uc_mcontext.gregs[REG_RIP];
#elif defined(__APPLE__)
    return context->uc_mcontext->__ss.__pc;
#else
    return context->uc_mcontext.pc;
#endif
}

void adv_posix_fault_set_pc(void *raw, uintptr_t pc) {
    ucontext_t *context = raw;
#if defined(__x86_64__)
    context->uc_mcontext.gregs[REG_RIP] = pc;
#elif defined(__APPLE__)
    context->uc_mcontext->__ss.__pc = pc;
#else
    context->uc_mcontext.pc = pc;
#endif
}

uint64_t adv_posix_fault_gpr(const void *raw, unsigned index) {
    const ucontext_t *context = raw;
#if defined(__x86_64__)
    static const int registers[16] = {
        REG_RAX, REG_RCX, REG_RDX, REG_RBX, REG_RSP, REG_RBP, REG_RSI, REG_RDI,
        REG_R8, REG_R9, REG_R10, REG_R11, REG_R12, REG_R13, REG_R14, REG_R15
    };
    return index < 16 ? (uint64_t)context->uc_mcontext.gregs[registers[index]] : 0;
#elif defined(__APPLE__)
    if (index < 29) return context->uc_mcontext->__ss.__x[index];
    if (index == 29) return context->uc_mcontext->__ss.__fp;
    if (index == 30) return context->uc_mcontext->__ss.__lr;
    return context->uc_mcontext->__ss.__sp;
#else
    return index < 31 ? context->uc_mcontext.regs[index] : context->uc_mcontext.sp;
#endif
}

uint64_t adv_posix_fault_flags(const void *raw) {
    const ucontext_t *context = raw;
#if defined(__x86_64__)
    return context->uc_mcontext.gregs[REG_EFL];
#elif defined(__APPLE__)
    return context->uc_mcontext->__ss.__cpsr;
#else
    return context->uc_mcontext.pstate;
#endif
}

int adv_posix_fault_fp(const void *raw, uint32_t *control, uint32_t *status,
                       unsigned char *vectors) {
    const ucontext_t *context = raw;
#if defined(__x86_64__)
    const struct _libc_fpstate *fp = context->uc_mcontext.fpregs;
    if (!fp) return 0;
    *control = fp->mxcsr;
    *status = 0;
    const unsigned char *source = (const unsigned char *)fp->_xmm;
    for (unsigned index = 0; index < 16 * 16; ++index) vectors[index] = source[index];
    for (unsigned index = 16 * 16; index < 32 * 16; ++index) vectors[index] = 0;
#elif defined(__APPLE__)
    if (!context->uc_mcontext) return 0;
    *control = context->uc_mcontext->__ns.__fpcr;
    *status = context->uc_mcontext->__ns.__fpsr;
    const unsigned char *source = (const unsigned char *)context->uc_mcontext->__ns.__v;
    for (unsigned index = 0; index < 32 * 16; ++index) vectors[index] = source[index];
#else
    const struct adv_fpsimd_context *fp = adv_fpsimd(context);
    if (!fp) return 0;
    *control = fp->fpcr;
    *status = fp->fpsr;
    const unsigned char *source = (const unsigned char *)fp->vregs;
    for (unsigned index = 0; index < 32 * 16; ++index) vectors[index] = source[index];
#endif
    return 1;
}

static uintptr_t adv_access(const ucontext_t *context) {
#if defined(__x86_64__)
    uint64_t error = context->uc_mcontext.gregs[REG_ERR];
    return error & 16 ? 2 : ((error >> 1) & 1);
#elif defined(__APPLE__)
    /* ESR WnR identifies data aborts; other exceptions stay unclaimed. */
    uint64_t esr = context->uc_mcontext->__es.__esr;
    return (esr >> 26) == 0x24 || (esr >> 26) == 0x25 ? (esr >> 6) & 1 : 2;
#else
    const unsigned char *records = context->uc_mcontext.__reserved;
    size_t offset = 0;
    while (offset <= sizeof(context->uc_mcontext.__reserved) - 8) {
        const struct adv_context_header *head = (const void *)(records + offset);
        if (!head->magic && !head->size) break;
        if (head->size < 16 || head->size % 16 ||
            head->size > sizeof(context->uc_mcontext.__reserved) - offset) break;
        if (head->magic == 0x45535201) {
            uint64_t esr = *(const uint64_t *)(records + offset + 8);
            return (esr >> 6) & 1;
        }
        offset += head->size;
    }
    /* Linux may omit ESR. The admitted side table identifies the access. */
    return UINTPTR_MAX;
#endif
}

int adv_posix_fault_synchronous(int signal, int code) {
    /* Positive si_code alone includes asynchronous MTE and machine-check
     * reports. Only the baseline synchronous memory-fault forms are admitted. */
    return signal == SIGSEGV ? (code == SEGV_MAPERR || code == SEGV_ACCERR) :
        (signal == SIGBUS && (code == BUS_ADRALN || code == BUS_ADRERR || code == BUS_OBJERR));
}

/* Darwin pthread_kill may label raise(SIGSEGV) as SEGV_ACCERR. Its saved
 * ESR describes SVC, not a data abort; do not consume an asynchronous signal
 * or retry it as a synchronous fault when restoring the old disposition. */
static int adv_synchronous(int signal, const siginfo_t *info, const void *raw) {
#ifdef __APPLE__
    if (adv_access(raw) > 1) return 0;
#else
    (void)raw;
#endif
    return adv_posix_fault_synchronous(signal, info->si_code);
}

static void adv_chain(int signal, siginfo_t *info, void *raw) {
    unsigned index = signal == SIGBUS;
    struct sigaction previous = adv_previous[index];
    if (previous.sa_flags & SA_RESETHAND) {
        if (atomic_exchange_explicit(&adv_reset[index], 1, memory_order_relaxed))
            previous.sa_handler = SIG_DFL;
    }
    if (previous.sa_handler == SIG_IGN && !adv_synchronous(signal, info, raw)) return;
    if (previous.sa_handler == SIG_DFL || previous.sa_handler == SIG_IGN) {
        /* Retry a synchronous fault under the original disposition. Ignoring
         * a synchronous memory fault has no defined useful continuation. */
        previous.sa_handler = SIG_DFL;
        previous.sa_flags = 0;
        sigaction(signal, &previous, NULL);
        if (!adv_synchronous(signal, info, raw)) kill(getpid(), signal);
        return;
    }
    if (previous.sa_flags & SA_SIGINFO) previous.sa_sigaction(signal, info, raw);
    else previous.sa_handler(signal);
}

static void adv_handler(int signal, siginfo_t *info, void *raw) {
    if (adv_synchronous(signal, info, raw) && adv_dispatch) {
        uintptr_t target = adv_dispatch(signal, adv_posix_fault_pc(raw),
            (uintptr_t)info->si_addr, adv_access(raw), raw);
        if (target) {
            adv_posix_fault_set_pc(raw, target);
            return;
        }
    }
    adv_chain(signal, info, raw);
}

int adv_posix_fault_install(adv_posix_dispatch_t dispatch) {
    struct sigaction actions[2];
    int signals[2] = {SIGSEGV, SIGBUS};
    for (unsigned index = 0; index < 2; ++index) {
        if (sigaction(signals[index], NULL, &adv_previous[index])) return 0;
        actions[index] = adv_previous[index];
        actions[index].sa_sigaction = adv_handler;
        actions[index].sa_flags = SA_SIGINFO |
            (adv_previous[index].sa_flags & (SA_ONSTACK | SA_RESTART | SA_NODEFER | SA_EXPOSE_TAGBITS));
    }
    adv_dispatch = dispatch;
    if (sigaction(SIGSEGV, &actions[0], NULL)) return 0;
    if (sigaction(SIGBUS, &actions[1], NULL)) {
        sigaction(SIGSEGV, &adv_previous[0], NULL);
        return 0;
    }
    return 1;
}
#endif
