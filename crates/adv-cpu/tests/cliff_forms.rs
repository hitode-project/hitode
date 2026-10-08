mod support;

use adv_cpu::{CompileStrategy, CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions};
use support::Executor;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const STRATEGIES: [CompileStrategy; 2] = [CompileStrategy::Fast, CompileStrategy::Speed];

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.enable_auto_jit(false);
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        // Faults must reenter at the instruction rather than replay its prefix.
        region.entries.extend(
            Decoder::with_ip(32, bytes, u64::from(CODE), DecoderOptions::NONE)
                .into_iter()
                .map(|i| i.ip32()),
        );
        m.add_region(region).unwrap();
    }
    m
}

fn initial() -> CpuState {
    CpuState {
        eip: CODE,
        regs: [
            0x89ab_cdef,
            0x7654_3210,
            0xabcd_ef01,
            0x1357_2468,
            DATA + 2048,
            DATA + 64,
            DATA + 128,
            DATA + 256,
        ],
        eflags: 0xed7,
        mxcsr: 0x5fc3,
        xmm: [[0x93; 16]; 8],
        ..CpuState::default()
    }
}

fn equal(native: &Executor, reference: &Executor) {
    let mut state = reference.state.clone();
    state.fuel = native.state.fuel;
    // The independent Unicorn prefix has not taken a fault. Fault addresses
    // are checked through RunOutcome; this scratch field is not guest state.
    state.fault_addr = native.state.fault_addr;
    assert_eq!(native.state, state);
    assert_eq!(
        native.memory().read(DATA, 8192).unwrap(),
        reference.memory().read(DATA, 8192).unwrap()
    );
}

fn finish(native: &mut Executor, interpreted: &mut Executor, oracle: &mut Executor, stop: u32) {
    assert_eq!(native.run(stop, 1000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        interpreted.run(stop, 1000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        oracle.run_unicorn(stop, 1000).unwrap(),
        RunOutcome::ReachedStop
    );
    equal(native, interpreted);
    equal(native, oracle);
    assert!(native.stats.native_entries > 0);
    assert_eq!(native.stats.verification.mismatches, 0);
}

fn carry_code(subtract: bool, form: usize, immediate: i32) -> Vec<u8> {
    code(|a| {
        macro_rules! emit {
            ($dst:expr, $src:expr) => {
                if subtract {
                    a.sbb($dst, $src)
                } else {
                    a.adc($dst, $src)
                }
                .unwrap()
            };
        }
        match form {
            0 => emit!(al, dl),
            1 => emit!(ah, dh),
            2 => emit!(ax, dx),
            3 => emit!(al, ah),
            4 => emit!(ah, al),
            5 => emit!(ax, ax),
            6 => emit!(al, immediate),
            7 => emit!(ah, immediate),
            8 => emit!(ax, immediate),
            9 => emit!(byte_ptr(esi), al),
            10 => emit!(word_ptr(esi), ax),
            11 => emit!(ah, byte_ptr(esi)),
            12 => emit!(ax, word_ptr(esi)),
            13 => emit!(byte_ptr(esi), immediate),
            _ => emit!(word_ptr(esi), immediate),
        }
        // INC preserves CF; the following ADC consumes its lazy value.
        a.inc(ebp).unwrap();
        a.adc(ebx, 0).unwrap();
    })
}

#[test]
fn narrow_carry_forms_match_interpreter_and_unicorn_flags_and_aliases() {
    for strategy in STRATEGIES {
        for subtract in [false, true] {
            for form in 0..15 {
                for immediate in [-1, 1, 127] {
                    let bytes = carry_code(subtract, form, immediate);
                    let stop = CODE + bytes.len() as u32;
                    let mut native = machine(&bytes, Some(strategy));
                    let mut interpreted = machine(&bytes, None);
                    let mut oracle = machine(&bytes, None);
                    for (a, b) in [
                        (0, 0),
                        (0, 1),
                        (1, 0xff),
                        (0xf, 1),
                        (0x7f, 0),
                        (0x7f, 1),
                        (0x80, 0xff),
                        (0xff, 0xff),
                        (0x7fff, 0),
                        (0x8000, 0xffff),
                        (0xffff, 1),
                    ] {
                        for carry in [0, 1] {
                            let mut state = initial();
                            let word = matches!(form, 2 | 5 | 8 | 10 | 12 | 14);
                            let lanes = |value| {
                                if word {
                                    value & 0xffff
                                } else {
                                    (value & 0xff) * 0x101
                                }
                            };
                            state.regs[0] = 0x89ab_0000 | lanes(a);
                            state.regs[2] = 0xabcd_0000 | lanes(b);
                            state.regs[6] = DATA + 4095;
                            state.eflags = 0xed6 | carry;
                            for m in [&mut native, &mut interpreted, &mut oracle] {
                                m.state = state.clone();
                                m.write(DATA, &[0x57; 8192]).unwrap();
                                m.write(state.regs[6], &(b as u16).to_le_bytes()).unwrap();
                            }
                            finish(&mut native, &mut interpreted, &mut oracle, stop);
                            assert_eq!(native.stats.interpreter_entries, 0);
                        }
                    }
                }
            }
        }
    }
}

fn exchange_code(form: usize) -> Vec<u8> {
    code(|a| {
        match form {
            0 => a.xchg(al, ah),
            1 => a.xchg(ah, al),
            2 => a.xchg(ah, dh),
            3 => a.xchg(ax, dx),
            4 => a.xchg(ax, ax),
            5 => a.xchg(byte_ptr(esi), ah),
            _ => a.xchg(word_ptr(esi), ax),
        }
        .unwrap()
    })
}

#[test]
fn narrow_exchange_preserves_flags_partial_registers_and_cross_page_memory() {
    for strategy in STRATEGIES {
        for form in 0..7 {
            let bytes = exchange_code(form);
            let mut native = machine(&bytes, Some(strategy));
            let mut interpreted = machine(&bytes, None);
            let mut oracle = machine(&bytes, None);
            for m in [&mut native, &mut interpreted, &mut oracle] {
                m.state = initial();
                m.state.regs[6] = DATA + 4095;
                m.write(DATA, &[0x57; 8192]).unwrap();
            }
            finish(
                &mut native,
                &mut interpreted,
                &mut oracle,
                CODE + bytes.len() as u32,
            );
            assert_eq!(native.stats.interpreter_entries, 0);
        }
    }
}

fn stack_code(pop: bool, word: bool) -> Vec<u8> {
    code(|a| {
        match (pop, word) {
            (false, true) => a.pusha(),
            (false, false) => a.pushad(),
            (true, true) => a.popa(),
            (true, false) => a.popad(),
        }
        .unwrap()
    })
}

#[test]
fn stack_all_widths_preserve_high_halves_original_sp_and_skipped_sp_slot() {
    for strategy in STRATEGIES {
        for pop in [false, true] {
            for word in [false, true] {
                let bytes = stack_code(pop, word);
                let mut native = machine(&bytes, Some(strategy));
                let mut interpreted = machine(&bytes, None);
                let mut oracle = machine(&bytes, None);
                for m in [&mut native, &mut interpreted, &mut oracle] {
                    m.state = initial();
                    m.state.regs[4] = DATA + 4093;
                    m.write(DATA, &(0..8192).map(|n| (n * 37) as u8).collect::<Vec<_>>())
                        .unwrap();
                }
                finish(
                    &mut native,
                    &mut interpreted,
                    &mut oracle,
                    CODE + bytes.len() as u32,
                );
                assert_eq!(native.stats.interpreter_entries, 0);
            }
        }
    }
}

#[test]
fn mxcsr_store_preserves_all_status_control_and_unrelated_state() {
    for strategy in STRATEGIES {
        let bytes = code(|a| a.stmxcsr(dword_ptr(esi).fs()).unwrap());
        let mut native = machine(&bytes, Some(strategy));
        let mut interpreted = machine(&bytes, None);
        let mut oracle = machine(&bytes, None);
        for mxcsr in [0, 0x1f80, 0x5fc3, 0xffff] {
            for m in [&mut native, &mut interpreted, &mut oracle] {
                m.state = initial();
                m.state.mxcsr = mxcsr;
                m.state.fs_base = DATA;
                m.state.regs[6] = 4092;
                m.write(DATA, &[0x57; 8192]).unwrap();
            }
            finish(
                &mut native,
                &mut interpreted,
                &mut oracle,
                CODE + bytes.len() as u32,
            );
            assert_eq!(native.stats.interpreter_entries, 0);
        }
    }
}

#[test]
fn new_memory_forms_fault_transactionally_and_resume_without_replaying_prefix() {
    let mut forms = Vec::new();
    for subtract in [false, true] {
        for form in 9..15 {
            forms.push(carry_code(subtract, form, -1));
        }
    }
    forms.extend([
        exchange_code(5),
        exchange_code(6),
        code(|a| a.stmxcsr(dword_ptr(esi)).unwrap()),
    ]);
    for strategy in STRATEGIES {
        for (form, tail) in forms.iter().enumerate() {
            let mut bytes = code(|a| {
                a.neg(ah).unwrap();
                a.mov(dword_ptr(DATA as i32 + 8192), eax).unwrap();
            });
            let fault_pc = CODE + bytes.len() as u32;
            bytes.extend(tail);
            for permission in [0, READ, WRITE] {
                for crossing in [false, true] {
                    let mut native = machine(&bytes, Some(strategy));
                    let mut interpreted = machine(&bytes, None);
                    let mut oracle = machine(&bytes, None);
                    for m in [&mut native, &mut interpreted, &mut oracle] {
                        m.state = initial();
                        m.state.regs[6] = DATA + if crossing { 4095 } else { 4096 };
                        m.write(DATA, &[0x57; 8192]).unwrap();
                        m.protect(DATA + 4096, 4096, permission).unwrap();
                    }
                    let outcome = native.run(CODE + bytes.len() as u32, 1000).unwrap();
                    assert_eq!(
                        outcome,
                        interpreted.run(CODE + bytes.len() as u32, 1000).unwrap(),
                        "form={form}, permission={permission}, crossing={crossing}, strategy={strategy:?}"
                    );
                    if outcome == RunOutcome::ReachedStop {
                        continue;
                    } // Byte accesses do not cross.
                    assert!(
                        matches!(outcome, RunOutcome::ReadFault {pc,..} | RunOutcome::WriteFault {pc,..} if pc == fault_pc)
                    );
                    assert_eq!(
                        oracle.run_unicorn(fault_pc, 1000).unwrap(),
                        RunOutcome::ReachedStop
                    );
                    equal(&native, &interpreted);
                    equal(&native, &oracle);
                    let entries = native.stats.interpreter_entries;
                    for m in [&mut native, &mut interpreted, &mut oracle] {
                        m.protect(DATA, 8192, READ | WRITE).unwrap();
                    }
                    finish(
                        &mut native,
                        &mut interpreted,
                        &mut oracle,
                        CODE + bytes.len() as u32,
                    );
                    assert_eq!(
                        native.stats.interpreter_entries,
                        entries + u64::from(form == 14 && crossing)
                    );
                }
            }
        }
    }
}

#[test]
fn stack_all_faults_commit_no_partial_registers_or_bytes_then_restart() {
    for strategy in STRATEGIES {
        for pop in [false, true] {
            for word in [false, true] {
                let width = if word { 2 } else { 4 };
                let bytes = stack_code(pop, word);
                for n in 0..8 {
                    let mut native = machine(&bytes, Some(strategy));
                    let mut interpreted = machine(&bytes, None);
                    let mut oracle = machine(&bytes, None);
                    for m in [&mut native, &mut interpreted, &mut oracle] {
                        m.state = initial();
                        m.state.regs[4] = if pop {
                            DATA + 4096 - n * width
                        } else {
                            DATA + 4096 + n * width
                        };
                        m.write(DATA, &[0x57; 8192]).unwrap();
                        m.protect(
                            if pop { DATA + 4096 } else { DATA },
                            4096,
                            if pop { WRITE } else { READ },
                        )
                        .unwrap();
                    }
                    let outcome = native.run(CODE + bytes.len() as u32, 1000).unwrap();
                    assert_eq!(
                        outcome,
                        interpreted.run(CODE + bytes.len() as u32, 1000).unwrap()
                    );
                    assert!(matches!(
                        outcome,
                        RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
                    ));
                    equal(&native, &interpreted);
                    equal(&native, &oracle);
                    let entries = native.stats.interpreter_entries;
                    for m in [&mut native, &mut interpreted, &mut oracle] {
                        m.protect(DATA, 8192, READ | WRITE).unwrap();
                    }
                    finish(
                        &mut native,
                        &mut interpreted,
                        &mut oracle,
                        CODE + bytes.len() as u32,
                    );
                    assert_eq!(native.stats.interpreter_entries, entries);
                }
            }
        }
    }
}

#[test]
fn release_integration_tests_enable_the_verifier_feature() {
    assert!(cfg!(feature = "jit-verifier"));
}

#[test]
fn strict_tier_verification_covers_the_new_native_forms() {
    let mut forms = vec![code(|a| a.stmxcsr(dword_ptr(esi)).unwrap())];
    for subtract in [false, true] {
        for form in 0..15 {
            forms.push(carry_code(subtract, form, -1));
        }
    }
    for form in 0..7 {
        forms.push(exchange_code(form));
    }
    for pop in [false, true] {
        for word in [false, true] {
            forms.push(stack_code(pop, word));
        }
    }
    for bytes in forms {
        let mut native = machine(&bytes, Some(CompileStrategy::Speed));
        native.set_tier_verification(Some(true));
        native.state = initial();
        native.write(DATA, &[0x57; 8192]).unwrap();
        assert_eq!(
            native.run(CODE + bytes.len() as u32, 1000).unwrap(),
            RunOutcome::ReachedStop
        );
        assert!(native.stats.verification.verified > 0);
        assert_eq!(native.stats.verification.mismatches, 0);
        assert!(native.stats.cliffs.fallbacks.is_empty());
    }
}

#[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
#[test]
fn narrow_memory_exchange_and_locked_carry_keep_the_parallel_atomic_fallback() {
    for width in [1, 2] {
        let bytes = code(|a| {
            if width == 1 {
                a.lock().adc(byte_ptr(esi), ah).unwrap();
                a.xchg(byte_ptr(esi), ah).unwrap();
            } else {
                a.lock().sbb(word_ptr(esi), ax).unwrap();
                a.xchg(word_ptr(esi), ax).unwrap();
            }
        });
        let mut native = machine(&bytes, None);
        native.set_guest_workers(2).unwrap();
        native.add_region(Region::new(CODE, bytes.len())).unwrap();
        let mut interpreted = machine(&bytes, None);
        for m in [&mut native, &mut interpreted] {
            m.state = initial();
            m.write(DATA, &[0x57; 8192]).unwrap();
        }
        let stop = CODE + bytes.len() as u32;
        assert_eq!(native.run(stop, 1000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(
            interpreted.run(stop, 1000).unwrap(),
            RunOutcome::ReachedStop
        );
        equal(&native, &interpreted);
        assert!(native.stats.interpreter_instructions >= 2);
    }
}
