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
fn rotates_match_unicorn_and_the_interpreter_at_zero_one_and_masked_counts() {
    let values = [
        0,
        1,
        0x4000_0000,
        0x1234_5678,
        0x7fff_ffff,
        0x8000_0000,
        0xffff_ffff,
    ];
    let counts = [0u8, 1, 2, 8, 16, 31, 32, 33, 255];
    for left in [true, false] {
        for immediate in [false, true] {
            for &count in &counts {
                let bytes = assemble(|a| {
                    match (left, immediate) {
                        (true, false) => a.rol(ebx, cl),
                        (true, true) => a.rol(ebx, count as i32),
                        (false, false) => a.ror(ebx, cl),
                        (false, true) => a.ror(ebx, count as i32),
                    }
                    .unwrap();
                    a.ret().unwrap();
                });
                let mut jit = machine(&bytes, true);
                let mut uc = machine(&bytes, false);
                let mut interpreted = machine(&bytes, false);
                for &value in &values {
                    for flags in [2, 0xcd7] {
                        let regs = [0, u32::from(count) | 0x1234_5600, 0, value, 0, 0, 0, 0];
                        for m in [&mut jit, &mut uc, &mut interpreted] {
                            reset(m, regs, flags);
                        }
                        // Rotates never define SF/ZF/AF/PF; OF only for one bit.
                        let mask = match count & 31 {
                            0 => 0xcd5,
                            1 => 0x8d5 | 0x400,
                            _ => 0xd5 | 0x400,
                        };
                        differential(&mut jit, &mut uc, mask);
                        assert_eq!(
                            interpreted.run(STOP, 10000).unwrap(),
                            RunOutcome::ReachedStop
                        );
                        // Fuel counts native blocks or interpreted instructions.
                        interpreted.state.fuel = jit.state.fuel;
                        assert_eq!(jit.state, interpreted.state, "value {value:#x}");
                    }
                }
                assert_eq!(
                    jit.stats.interpreter_entries, 0,
                    "rotate left {left}, immediate {immediate}, count {count}"
                );
            }
        }
    }
}

#[test]
fn loop_decrements_ecx_without_flags_and_matches_unicorn() {
    let bytes = assemble(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.add(eax, ecx).unwrap();
        a.cmp(eax, 3).unwrap();
        a.loop_(top).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&bytes, true);
    let mut uc = machine(&bytes, false);
    // ECX = 0 wraps and runs 2^32 times; keep the counts finite.
    for count in [1, 2, 7, 300] {
        for flags in [2, 0xcd7] {
            let regs = [0, count, 0, 0, 0, 0, 0, 0];
            reset(&mut jit, regs, flags);
            reset(&mut uc, regs, flags);
            differential(&mut jit, &mut uc, 0xcd5);
            assert_eq!(jit.state.regs[1], 0);
        }
    }
    assert_eq!(jit.stats.interpreter_entries, 0);
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

#[test]
fn flag_transfers_direction_and_byte_swap_match_unicorn() {
    let values = [0, 1, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 0x1234_d5c7];
    // EFLAGS images without TF, which would single-step.
    let images = [0, 0xffff_feff, 0x0000_0ed7, 0x0024_4cd5, 0x8d5];
    for form in 0..9 {
        let bytes = assemble(|a| {
            // A lazy producer first, so each form must see derived flags.
            a.add(eax, ebx).unwrap();
            match form {
                0 => a.lahf().unwrap(),
                1 => {
                    a.mov(eax, edx).unwrap();
                    a.sahf().unwrap();
                }
                2 => {
                    a.pushfd().unwrap();
                    a.pop(ecx).unwrap();
                }
                3 => {
                    a.pushf().unwrap();
                    a.movzx(ecx, word_ptr(esp)).unwrap();
                    a.add(esp, 2).unwrap();
                }
                4 => {
                    a.push(edx).unwrap();
                    a.popfd().unwrap();
                }
                5 => {
                    a.push(dx).unwrap();
                    a.popf().unwrap();
                }
                6 => {
                    a.std().unwrap();
                    a.pushfd().unwrap();
                    a.pop(ecx).unwrap();
                    a.cld().unwrap();
                }
                7 => a.bswap(eax).unwrap(),
                _ => {
                    a.mov(eax, edx).unwrap();
                    a.sahf().unwrap();
                    a.seto(cl).unwrap();
                    a.setc(ch).unwrap();
                }
            }
            a.ret().unwrap();
        });
        let mut jit = machine(&bytes, true);
        let mut uc = machine(&bytes, false);
        for &left in &values {
            for &right in &values {
                for &image in &images {
                    let context = format!("form {form}, {left:#x} + {right:#x}, image {image:#x}");
                    let regs = [left, 0, image, right, 0, 0, 0, 0];
                    reset(&mut jit, regs, 0x202);
                    assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
                    // Unicorn for the results; the interpreter for EFLAGS, which
                    // Unicorn reports stale after PUSHFD.
                    reset(&mut uc, regs, 0x202);
                    assert_eq!(
                        uc.run_unicorn(STOP, 10000).unwrap(),
                        RunOutcome::ReachedStop
                    );
                    assert_eq!(jit.state.regs, uc.state.regs, "{context}");
                    reset(&mut uc, regs, 0x202);
                    assert_eq!(uc.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(jit.state.regs, uc.state.regs, "{context}");
                    assert_eq!(jit.state.eflags, uc.state.eflags, "{context}");
                }
            }
        }
        assert_eq!(jit.stats.interpreter_entries, 0, "form {form}");
    }
}
