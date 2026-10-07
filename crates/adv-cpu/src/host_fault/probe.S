/* Synthetic native fixture: a visible prefix, live registers/flags/FP state,
 * then one scalar operation. Recovery calls Rust in the same native frame. */
#if defined(__x86_64__)
.text
.p2align 4
.globl adv_posix_probe_start
.globl adv_posix_probe_load
.globl adv_posix_probe_store
.globl adv_posix_probe_recovery
.globl adv_posix_probe_chain_recovery
.globl adv_posix_probe_end
.type adv_posix_probe_start, @function
adv_posix_probe_start:
    movl %esi, %esi
    pushq %rbx
    pushq %rbp
    pushq %r12
    pushq %r13
    pushq %r14
    pushq %r15
    subq $8, %rsp
    stmxcsr (%rsp)
    movl $0x5f80, 4(%rsp)
    ldmxcsr 4(%rsp)
    movdqa adv_posix_probe_pattern(%rip), %xmm8
    incl (%rcx)
    movl $0x21, %r10d
    movl $0x32, %r11d
    movl $0x43, %ebx
    movl $0x65, %ebp
    movl $0x76, %r12d
    movl $0x87, %r13d
    movl $0x7fffffff, %eax
    addl $1, %eax
    pushfq
    popq %r14
    testl %r8d, %r8d
    jnz 1f
    /* TEST above is followed by restoring the exact fault-point flags. */
    pushq %r14
    popfq
adv_posix_probe_load:
    movl (%rdi,%rsi), %r15d
    movl %r15d, %eax
    jmp 2f
1:
    pushq %r14
    popfq
adv_posix_probe_store:
    movl %edx, (%rdi,%rsi)
    movl %edx, %eax
    jmp 2f
adv_posix_probe_recovery:
    call *%r9
    addl $1, %eax
    jmp 2f
adv_posix_probe_chain_recovery:
    movl $0xface, %eax
2:
    ldmxcsr (%rsp)
    addq $8, %rsp
    popq %r15
    popq %r14
    popq %r13
    popq %r12
    popq %rbp
    popq %rbx
    ret
adv_posix_probe_end:
.size adv_posix_probe_start, adv_posix_probe_end-adv_posix_probe_start
.section .rodata
.p2align 4
adv_posix_probe_pattern:
    .fill 16,1,0x5a
#elif defined(__aarch64__)
.text
.p2align 4
.globl adv_posix_probe_start
.globl adv_posix_probe_load
.globl adv_posix_probe_store
.globl adv_posix_probe_recovery
.globl adv_posix_probe_chain_recovery
.globl adv_posix_probe_end
.type adv_posix_probe_start, %function
adv_posix_probe_start:
    stp x19,x20,[sp,#-128]!
    stp x21,x22,[sp,#16]
    stp x23,x24,[sp,#32]
    stp x25,x26,[sp,#48]
    stp x27,x28,[sp,#64]
    stp x29,x30,[sp,#80]
    str d8,[sp,#96]
    mrs x12,fpcr
    str x12,[sp,#104]
    mrs x12,fpsr
    str x12,[sp,#112]
    mov x12,#0x400000
    msr fpcr,x12
    mov x12,#0x10
    msr fpsr,x12
    movi v16.16b,#0x5a
    mov x12,#0x1234
    fmov d8,x12
    ldr w12,[x3]
    add w12,w12,#1
    str w12,[x3]
    mov w9,w1
    add x10,x0,x9
    mov w11,w2
    mov x15,x5
    mov w20,#0x21
    mov w21,#0x32
    mov w22,#0x43
    mov w23,#0x54
    mov w24,#0x65
    mov w25,#0x76
    mov w26,#0x87
    mov w19,#0x7fffffff
    adds w19,w19,#1
    mov w8,#0x896
    cbnz w4,1f
adv_posix_probe_load:
    ldr w12,[x10]
    mov w0,w12
    b 2f
1:
adv_posix_probe_store:
    str w11,[x10]
    mov w0,w11
    b 2f
adv_posix_probe_recovery:
    blr x15
    add w0,w0,#1
    b 2f
adv_posix_probe_chain_recovery:
    mov w0,#0xface
2:
    ldr x12,[sp,#104]
    msr fpcr,x12
    ldr x12,[sp,#112]
    msr fpsr,x12
    ldr d8,[sp,#96]
    ldp x29,x30,[sp,#80]
    ldp x27,x28,[sp,#64]
    ldp x25,x26,[sp,#48]
    ldp x23,x24,[sp,#32]
    ldp x21,x22,[sp,#16]
    ldp x19,x20,[sp],#128
    ret
adv_posix_probe_end:
.size adv_posix_probe_start, adv_posix_probe_end-adv_posix_probe_start
#endif
.section .note.GNU-stack,"",@progbits
