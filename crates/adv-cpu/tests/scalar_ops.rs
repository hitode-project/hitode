mod support;
use adv_cpu::{CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;

fn assemble(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}
fn machine(bytes: &[u8], jit: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if jit {
        m.set_region(Region::new(CODE, bytes.len())).unwrap();
    }
    m
}
fn reset(m: &mut Executor, regs: [u32; 8], flags: u32) {
    m.state = CpuState {
        regs,
        eflags: flags,
        eip: CODE,
        ..CpuState::default()
    };
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}
fn differential(jit: &mut Executor, uc: &mut Executor, flags_mask: u32) {
    assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        uc.run_unicorn(STOP, 10000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, uc.state.regs);
    assert_eq!(jit.state.eflags & flags_mask, uc.state.eflags & flags_mask);
}

#[test]
fn multiply_forms_match_unicorn_results_and_defined_flags() {
    let values = [
        0,
        1,
        2,
        0x7fff,
        0x10000,
        0x7fff_ffff,
        0x8000_0000,
        0xffff_fffe,
        0xffff_ffff,
    ];
    for form in 0..8 {
        let bytes = assemble(|a| {
            match form {
                0 => a.mul(ecx),
                1 => a.imul(ecx),
                2 => a.imul_2(ebx, ecx),
                3 => a.imul_3(ebx, ecx, -7),
                4 => a.imul_3(ebx, ecx, 0x1234_5678),
                5 => a.mul(dword_ptr(DATA as i32)),
                6 => a.imul(dword_ptr(DATA as i32)),
                _ => a.imul_2(ebx, dword_ptr(DATA as i32)),
            }
            .unwrap();
            // Read CF and OF through native branch conditions before a state export.
            let mut carry = a.create_label();
            let mut after_carry = a.create_label();
            let mut overflow = a.create_label();
            let mut done = a.create_label();
            a.jb(carry).unwrap();
            a.mov(esi, 0).unwrap();
            a.jmp(after_carry).unwrap();
            a.set_label(&mut carry).unwrap();
            a.mov(esi, 1).unwrap();
            a.set_label(&mut after_carry).unwrap();
            a.jo(overflow).unwrap();
            a.mov(edi, 0).unwrap();
            a.jmp(done).unwrap();
            a.set_label(&mut overflow).unwrap();
            a.mov(edi, 1).unwrap();
            a.set_label(&mut done).unwrap();
            a.ret().unwrap();
        });
        let mut jit = machine(&bytes, true);
        let mut uc = machine(&bytes, false);
        for &left in &values {
            for &right in &values {
                for flags in [2, 0xcd7] {
                    for m in [&mut jit, &mut uc] {
                        reset(m, [left, right, 0x9876_5432, left, 0, 0, 0, 0], flags);
                        m.write(DATA, &right.to_le_bytes()).unwrap();
                    }
                    differential(&mut jit, &mut uc, 0x801 | 0x400);
                }
            }
        }
        assert_eq!(jit.stats.interpreter_entries, 0, "multiply form {form}");
    }
}

#[test]
fn shifts_match_unicorn_at_zero_one_and_masked_counts() {
    let values = [0, 1, 2, 0x1234_5678, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff];
    let counts = [0u8, 1, 2, 7, 15, 31, 32, 33, 63, 255];
    for operation in 0..3 {
        for immediate in [false, true] {
            for &count in &counts {
                let bytes = assemble(|a| {
                    match (operation, immediate) {
                        (0, false) => a.shl(eax, cl),
                        (0, true) => a.shl(eax, count as i32),
                        (1, false) => a.shr(eax, cl),
                        (1, true) => a.shr(eax, count as i32),
                        (_, false) => a.sar(eax, cl),
                        (_, true) => a.sar(eax, count as i32),
                    }
                    .unwrap();
                    a.ret().unwrap();
                });
                let mut jit = machine(&bytes, true);
                let mut uc = machine(&bytes, false);
                for &value in &values {
                    for flags in [2, 0xcd7] {
                        let regs = [value, u32::from(count) | 0x1234_5600, 0, 0, 0, 0, 0, 0];
                        reset(&mut jit, regs, flags);
                        reset(&mut uc, regs, flags);
                        let mask = match count & 31 {
                            0 => 0xcd5,
                            1 => 0x8c5 | 0x400,
                            _ => 0xc5 | 0x400,
                        };
                        differential(&mut jit, &mut uc, mask);
                    }
                }
                assert_eq!(
                    jit.stats.interpreter_entries, 0,
                    "shift {operation}, immediate {immediate}, count {count}"
                );
            }
        }
    }
}

#[test]
fn multiplication_memory_fault_is_atomic_and_short_forms_fall_back() {
    let bytes = assemble(|a| {
        a.mul(dword_ptr(edi)).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&bytes, true);
    jit.protect(DATA + 4096, 4096, 0).unwrap();
    reset(&mut jit, [7, 9, 11, 13, 0, 17, 19, DATA + 4094], 0xcd7);
    let before = jit.state.clone();
    assert_eq!(
        jit.run(STOP, 100).unwrap(),
        RunOutcome::ReadFault {
            pc: CODE,
            address: DATA + 4094
        }
    );
    assert_eq!(jit.state.regs, before.regs);
    assert_eq!(jit.state.eflags, before.eflags);
    let bytes = assemble(|a| {
        a.imul(ax).unwrap();
        a.shl(ax, cl).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&bytes, true);
    let mut uc = machine(&bytes, false);
    let regs = [0x1234_7fff, 2, 0xabcd_9876, 0, 0, 0, 0, 0];
    reset(&mut jit, regs, 2);
    reset(&mut uc, regs, 2);
    // SHL by two leaves OF undefined; only its defined CF/SF/ZF/PF are an oracle.
    differential(&mut jit, &mut uc, 0xc5);
    assert!(jit.stats.interpreter_entries > 0);
}

#[test]
fn byte_and_word_compare_test_flags_and_branches_match_unicorn() {
    for subtract in [false, true] {
        for form in 0..8 {
            let bytes = assemble(|a| {
                match (subtract, form) {
                    (false, 0) => a.test(al, cl),
                    (true, 0) => a.cmp(al, cl),
                    (false, 1) => a.test(ah, cl),
                    (true, 1) => a.cmp(ah, cl),
                    (false, 2) => a.test(ax, cx),
                    (true, 2) => a.cmp(ax, cx),
                    (false, 3) => a.test(al, 0x80),
                    (true, 3) => a.cmp(al, 0x80),
                    (false, 4) => a.test(ax, 0x8000),
                    (true, 4) => a.cmp(ax, 0x8000),
                    (false, 5) => a.test(byte_ptr(DATA as i32), cl),
                    (true, 5) => a.cmp(byte_ptr(DATA as i32), cl),
                    (false, 6) => a.test(word_ptr(DATA as i32), cx),
                    (true, 6) => a.cmp(word_ptr(DATA as i32), cx),
                    (false, _) => a.test(ax, 0xfff9),
                    (true, _) => a.cmp(ax, -7),
                }
                .unwrap();
                let mut yes = a.create_label();
                let mut done = a.create_label();
                a.jl(yes).unwrap();
                a.mov(esi, 0).unwrap();
                a.jmp(done).unwrap();
                a.set_label(&mut yes).unwrap();
                a.mov(esi, 1).unwrap();
                a.set_label(&mut done).unwrap();
                a.ret().unwrap();
            });
            let mut jit = machine(&bytes, true);
            let mut uc = machine(&bytes, false);
            let values = [0u32, 1, 15, 127, 128, 255, 0x7fff, 0x8000, 0xffff];
            for &left in &values {
                for &right in &values {
                    let eax_value = if form == 1 {
                        0x1234_0003 | (left & 255) << 8
                    } else {
                        0x1234_0000 | left
                    };
                    for m in [&mut jit, &mut uc] {
                        reset(m, [eax_value, 0x9876_0000 | right, 0, 0, 0, 0, 0, 0], 0xcd7);
                        m.write(DATA, &left.to_le_bytes()).unwrap();
                    }
                    differential(&mut jit, &mut uc, if subtract { 0xcd5 } else { 0xcc5 });
                }
            }
            assert_eq!(
                jit.stats.interpreter_entries, 0,
                "subtract={subtract}, form={form}"
            );
        }
    }
}
