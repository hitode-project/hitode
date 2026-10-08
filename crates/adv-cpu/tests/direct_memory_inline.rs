//! Synthetic direct-memory contracts on supported POSIX hosts.
//! Run this binary with ADV_DIRECT_MEMORY_INLINE unset or 1; no test mutates
//! process environment. The interpreter oracle is forced through the public API.
#![cfg(all(
    any(
        target_os = "linux",
        target_os = "android",
        all(target_os = "macos", target_arch = "aarch64")
    ),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use adv_cpu::{
    CompileStrategy, CpuState, EXEC, ExecutionTrap, Executor, JitCpu, Memory, READ, Region,
    RunOutcome, WRITE, X87Register,
};
use iced_x86::{Decoder, DecoderOptions, code_asm::*};

const CODE: u32 = 0x10000;
const NEXT: u32 = 0x12000;
const DATA: u32 = 0x20000;
const FAULT: u32 = 0x30000;
const DEST: u32 = 0x40000;
const PAGE: usize = 4096;
const BUDGET: u64 = 100;

struct NoTraps;

impl ExecutionTrap for NoTraps {
    fn call(&mut self, _: &mut CpuState, _: &mut Memory, _: u32) -> Result<bool, String> {
        Err("unexpected synthetic trap".into())
    }
}

fn interpret(m: &mut Executor, stop: u32, budget: u64) -> RunOutcome {
    m.run_with_traps(stop, budget, &mut NoTraps, true).unwrap()
}

fn program(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    assemble(CODE, build)
}

fn assemble(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn seed() -> CpuState {
    let mut state = CpuState {
        eip: CODE,
        eflags: 0x202,
        regs: [
            0xaabb_ccdd,
            17,
            0x89ab_cdef,
            u32::MAX,
            DEST + 1024,
            11,
            DATA,
            DEST,
        ],
        fs_base: 0x50000,
        mxcsr: 0x5f80,
        ..CpuState::default()
    };
    state.xmm = std::array::from_fn(|r| std::array::from_fn(|b| (r * 19 + b * 7) as u8));
    state.x87.control = 0x077f;
    state.x87.status = 0x2800;
    state.x87.regs =
        std::array::from_fn(|r| X87Register::from_parts(0x8000_0000_0000_0000 | r as u64, 0x3fff));
    state.segments.gdt_base = 0x60000;
    state.segments.gdt_limit = 127;
    state.segments.idt_base = 0x61000;
    state.segments.idt_limit = 255;
    state
}

fn configure(m: &mut Executor, native: bool) {
    m.disable_row_batching();
    m.enable_auto_jit(false);
    m.set_async_compilation(false);
    m.set_tiered_compilation(false);
    if native {
        m.set_direct_memory(true).unwrap();
        m.set_tier_verification(Some(true));
    } else {
        m.set_tier_verification(None);
    }
}

fn machine(code: &[u8], native: bool, strategy: CompileStrategy) -> Executor {
    let mut m = Executor::address_space().unwrap();
    configure(&mut m, native);
    m.map(CODE, PAGE, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.protect(CODE, PAGE, READ | EXEC).unwrap();
    for at in [DATA, DEST] {
        m.map(at, 2 * PAGE, READ | WRITE).unwrap();
        let bytes: Vec<_> = (0..2 * PAGE).map(|i| (i * 37 + 13) as u8).collect();
        m.write(at, &bytes).unwrap();
    }
    if native {
        let mut region = Region::new(CODE, code.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    m.state = seed();
    m
}

fn pair(code: &[u8]) -> [Executor; 2] {
    [
        machine(code, false, CompileStrategy::Speed),
        machine(code, true, CompileStrategy::Speed),
    ]
}

fn equal_state(reference: &Executor, direct: &Executor) {
    // Native fuel counts basic blocks, while the oracle counts instructions.
    // fault_addr is diagnostic scratch outside a reported fault; fault tests
    // assert it separately. All other fields, including raw flags, are exact.
    let mut expected = reference.state.clone();
    let mut actual = direct.state.clone();
    expected.fuel = 0;
    actual.fuel = 0;
    expected.fault_addr = 0;
    actual.fault_addr = 0;
    assert_eq!(actual, expected);
    assert_eq!(direct.stats.verification.mismatches, 0);
}

fn equal_memory(reference: &Executor, direct: &Executor, address: u32, len: usize) {
    assert_eq!(
        direct.memory().read_data(address, len).unwrap().as_ref(),
        reference.memory().read_data(address, len).unwrap().as_ref()
    );
}

fn load(a: &mut CodeAssembler, width: usize) {
    match width {
        1 => a.movzx(eax, byte_ptr(edi)).unwrap(),
        2 => a.movzx(eax, word_ptr(edi)).unwrap(),
        4 => a.mov(eax, dword_ptr(edi)).unwrap(),
        _ => unreachable!(),
    }
}

fn store(a: &mut CodeAssembler, width: usize) {
    match width {
        1 => a.mov(byte_ptr(edi), dl).unwrap(),
        2 => a.mov(word_ptr(edi), dx).unwrap(),
        4 => a.mov(dword_ptr(edi), edx).unwrap(),
        _ => unreachable!(),
    }
}

#[test]
fn scalar_width_loops_and_partial_register_neighbors_match_interpreter() {
    for width in [1, 2, 4] {
        let code = program(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            load(a, width);
            a.mov(edx, eax).unwrap();
            a.xor(edx, 0x1357_9bdf).unwrap();
            // The second base advances too; no invariant load/store slot can
            // replace the actual accesses in this loop.
            match width {
                1 => a.mov(byte_ptr(esi), dl).unwrap(),
                2 => a.mov(word_ptr(esi), dx).unwrap(),
                4 => a.mov(dword_ptr(esi), edx).unwrap(),
                _ => unreachable!(),
            }
            a.lea(edi, ptr(edi + 8)).unwrap();
            a.lea(esi, ptr(esi + 8)).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            // Narrow MOV retains the upper register bits, unlike MOVZX.
            a.mov(eax, 0xaabb_ccddu32).unwrap();
            a.mov(al, byte_ptr(edi)).unwrap();
            a.mov(ax, word_ptr(edi + 2)).unwrap();
        });
        let stop = CODE + code.len() as u32;
        for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
            for offset in [32, 33, 4094] {
                let mut oracle = machine(&code, false, strategy);
                let mut direct = machine(&code, true, strategy);
                for m in [&mut oracle, &mut direct] {
                    m.state.regs[1] = 17;
                    m.state.regs[6] = DEST + offset;
                    m.state.regs[7] = DATA + offset;
                    // Loading a read-only source must remain legal.
                    m.protect(DATA, 2 * PAGE, READ).unwrap();
                }
                assert_eq!(interpret(&mut oracle, stop, 1000), RunOutcome::ReachedStop);
                assert_eq!(direct.run(stop, 1000).unwrap(), RunOutcome::ReachedStop);
                equal_state(&oracle, &direct);
                equal_memory(&oracle, &direct, DEST, 2 * PAGE);
                assert_eq!(direct.state.regs[0] & 0xffff_0000, 0xaabb_0000);
                assert!(direct.stats.native_entries > 0);
                assert!(direct.stats.verification.verified > 0);
            }
        }
    }
}

#[test]
fn scalar_faults_preserve_committed_prefix_flags_and_resume_without_replay() {
    for width in [1, 2, 4] {
        for write in [false, true] {
            let code = program(|a| {
                a.add(ebx, 1).unwrap();
                a.mov(dword_ptr(esi), edx).unwrap();
                if write {
                    store(a, width);
                } else {
                    load(a, width);
                }
                a.inc(ebp).unwrap();
            });
            let instructions: Vec<_> = Decoder::with_ip(32, &code, u64::from(CODE), 0)
                .into_iter()
                .collect();
            let fault_pc = instructions[2].ip32();
            let stop = CODE + code.len() as u32;
            // Denied, read-only/write-only, and genuinely unmapped are distinct
            // metadata shapes; none may be granted by a stale direct alias.
            for protection in [Some(0), Some(if write { READ } else { WRITE }), None] {
                let [mut oracle, mut direct] = pair(&code);
                for m in [&mut oracle, &mut direct] {
                    m.map(FAULT, PAGE, READ | WRITE).unwrap();
                    m.write(FAULT, &[0x55; 4]).unwrap();
                    if let Some(perms) = protection {
                        m.protect(FAULT, PAGE, perms).unwrap();
                    } else {
                        m.unmap(FAULT, PAGE).unwrap();
                    }
                    m.state.regs[7] = FAULT;
                }
                let expected = if write {
                    RunOutcome::WriteFault {
                        pc: fault_pc,
                        address: FAULT,
                    }
                } else {
                    RunOutcome::ReadFault {
                        pc: fault_pc,
                        address: FAULT,
                    }
                };
                assert_eq!(interpret(&mut oracle, stop, BUDGET), expected);
                assert_eq!(direct.run(stop, BUDGET).unwrap(), expected);
                equal_state(&oracle, &direct);
                assert_eq!(direct.state.eip, fault_pc);
                assert_eq!(direct.state.fault_addr, FAULT);
                assert_eq!(direct.state.regs[0], 0xaabb_ccdd);
                assert_eq!(direct.state.regs[3], 0);
                assert_eq!(direct.state.regs[5], 11);
                assert_eq!(direct.state.eflags, 0x257);
                assert_eq!(oracle.state.fuel, BUDGET - 2);
                assert_eq!(direct.state.fuel, BUDGET - 1);
                equal_memory(&oracle, &direct, DATA, 2 * PAGE);
                assert_eq!(
                    direct.memory().read(DATA, 4).unwrap(),
                    &0x89ab_cdefu32.to_le_bytes()
                );
                for m in [&mut oracle, &mut direct] {
                    // A replayed prefix store would overwrite this marker.
                    m.write(DATA, &[0x77; 4]).unwrap();
                    if protection.is_some() {
                        m.protect(FAULT, PAGE, READ | WRITE).unwrap();
                        assert_eq!(m.memory().read(FAULT, 4).unwrap(), &[0x55; 4]);
                    } else {
                        m.map(FAULT, PAGE, READ | WRITE).unwrap();
                    }
                }
                assert_eq!(
                    interpret(&mut oracle, stop, BUDGET),
                    RunOutcome::ReachedStop
                );
                assert_eq!(direct.run(stop, BUDGET).unwrap(), RunOutcome::ReachedStop);
                equal_state(&oracle, &direct);
                equal_memory(&oracle, &direct, FAULT, PAGE);
                assert_eq!(direct.state.regs[3], 0);
                assert_eq!(direct.state.regs[5], 12);
                assert_eq!(direct.memory().read(DATA, 4).unwrap(), &[0x77; 4]);
                assert!(direct.stats.native_entries > 0);
            }
        }
    }
}

#[test]
fn exhausted_native_fuel_keeps_one_uncommitted_mov_pending() {
    // Larger host pages deliberately use checked guards rather than hardware
    // fault continuation. Their memory semantics are covered by the other tests.
    // SAFETY: sysconf reads the page size without pointer arguments.
    if unsafe { libc::sysconf(libc::_SC_PAGESIZE) } != PAGE as libc::c_long {
        return;
    }
    let code = program(|a| {
        a.add(ebx, 1).unwrap();
        a.mov(dword_ptr(esi), edx).unwrap();
        a.mov(dword_ptr(edi), edx).unwrap();
        a.inc(ebp).unwrap();
    });
    let instructions: Vec<_> = Decoder::with_ip(32, &code, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .collect();
    let stop = CODE + code.len() as u32;
    // Separate single-instruction regions make the committed-prefix fuel
    // directly comparable with the instruction-counted oracle.
    let mut oracle = machine(&code, false, CompileStrategy::Speed);
    let mut direct = machine(&code, false, CompileStrategy::Speed);
    configure(&mut direct, true);
    for i in &instructions {
        direct.add_region(Region::new(i.ip32(), i.len())).unwrap();
    }
    for m in [&mut oracle, &mut direct] {
        m.map(FAULT, PAGE, READ).unwrap();
        m.state.regs[7] = FAULT;
    }
    assert_eq!(interpret(&mut oracle, stop, 2), RunOutcome::Budget);
    assert_eq!(direct.run(stop, 2).unwrap(), RunOutcome::Budget);
    assert_eq!(direct.state, oracle.state);
    equal_memory(&oracle, &direct, DATA, PAGE);
    let committed = direct.state.clone();
    assert_eq!(direct.run(stop, 1).unwrap(), RunOutcome::Budget);
    // The guard spent the last native block unit; no part of the MOV committed.
    equal_state(&oracle, &direct);
    assert_eq!(direct.state.fuel, committed.fuel);
    equal_memory(&oracle, &direct, DATA, PAGE);
    let expected = RunOutcome::WriteFault {
        pc: instructions[2].ip32(),
        address: FAULT,
    };
    assert_eq!(interpret(&mut oracle, stop, 1), expected);
    assert_eq!(direct.run(stop, 1).unwrap(), expected);
    assert_eq!(direct.state, oracle.state);
    assert_eq!(direct.state.fuel, 1);
    for m in [&mut oracle, &mut direct] {
        m.protect(FAULT, PAGE, READ | WRITE).unwrap();
        m.write(DATA, &[0x77; 4]).unwrap();
    }
    assert_eq!(
        interpret(&mut oracle, stop, BUDGET),
        RunOutcome::ReachedStop
    );
    assert_eq!(direct.run(stop, BUDGET).unwrap(), RunOutcome::ReachedStop);
    equal_state(&oracle, &direct);
    assert_eq!(direct.memory().read(DATA, 4).unwrap(), &[0x77; 4]);
    assert_eq!(direct.stats.verification.mismatches, 0);
}

#[test]
fn unaligned_and_cross_page_stores_never_commit_a_denied_prefix() {
    for width in [2, 4] {
        let code = program(|a| {
            a.add(ebx, 1).unwrap();
            store(a, width);
            a.inc(ebp).unwrap();
        });
        let stop = CODE + code.len() as u32;
        for (offset, suffix) in [
            (1, Some(READ | WRITE)),
            (PAGE as u32 - width as u32 / 2, Some(READ | WRITE)),
            (PAGE as u32 - width as u32 / 2, Some(READ)),
            (PAGE as u32 - width as u32 / 2, None),
        ] {
            let [mut oracle, mut direct] = pair(&code);
            for m in [&mut oracle, &mut direct] {
                m.state.regs[7] = DEST + offset;
                if let Some(perms) = suffix {
                    m.protect(DEST + PAGE as u32, PAGE, perms).unwrap();
                } else {
                    m.unmap(DEST + PAGE as u32, PAGE).unwrap();
                }
            }
            let before = direct.memory().read(DEST, PAGE).unwrap().to_vec();
            let expected = interpret(&mut oracle, stop, BUDGET);
            assert_eq!(direct.run(stop, BUDGET).unwrap(), expected);
            equal_state(&oracle, &direct);
            equal_memory(&oracle, &direct, DEST, PAGE);
            if suffix == Some(READ | WRITE) {
                assert_eq!(expected, RunOutcome::ReachedStop);
            } else {
                assert!(
                    matches!(expected, RunOutcome::WriteFault { address, .. } if address == DEST + offset)
                );
                assert_eq!(direct.memory().read(DEST, PAGE).unwrap(), before.as_slice());
                assert_eq!(direct.state.regs[3], 0);
                assert_eq!(direct.state.regs[5], 11);
                for m in [&mut oracle, &mut direct] {
                    if suffix.is_some() {
                        m.protect(DEST + PAGE as u32, PAGE, READ | WRITE).unwrap();
                    } else {
                        m.map(DEST + PAGE as u32, PAGE, READ | WRITE).unwrap();
                    }
                }
                assert_eq!(
                    interpret(&mut oracle, stop, BUDGET),
                    RunOutcome::ReachedStop
                );
                assert_eq!(direct.run(stop, BUDGET).unwrap(), RunOutcome::ReachedStop);
                equal_state(&oracle, &direct);
                assert_eq!(direct.state.regs[3], 0);
            }
            equal_memory(&oracle, &direct, DEST, 2 * PAGE);
            assert_eq!(
                direct.memory().read(DEST + offset, width).unwrap(),
                &0x89ab_cdefu32.to_le_bytes()[..width]
            );
        }
    }
}

#[test]
fn hardware_fault_checkpoint_preserves_dirty_simd_and_x87_state() {
    let code = program(|a| {
        a.movd(xmm0, edx).unwrap();
        a.paddd(xmm1, xmm0).unwrap();
        a.fld1().unwrap();
        a.add(ebx, 1).unwrap();
        a.mov(eax, dword_ptr(edi)).unwrap();
        a.fstp(st0).unwrap();
    });
    let stop = CODE + code.len() as u32;
    let [mut oracle, mut direct] = pair(&code);
    for m in [&mut oracle, &mut direct] {
        m.state.regs[7] = FAULT;
    }
    let expected = interpret(&mut oracle, stop, BUDGET);
    assert!(matches!(
        expected,
        RunOutcome::ReadFault { address: FAULT, .. }
    ));
    assert_eq!(direct.run(stop, BUDGET).unwrap(), expected);
    equal_state(&oracle, &direct);
    assert_ne!(direct.state.xmm[0], seed().xmm[0]);
    assert_ne!(direct.state.x87.status, seed().x87.status);
    assert!(direct.stats.native_entries > 0);
    for m in [&mut oracle, &mut direct] {
        m.map(FAULT, PAGE, READ | WRITE).unwrap();
        m.write(FAULT, &42u32.to_le_bytes()).unwrap();
    }
    assert_eq!(
        interpret(&mut oracle, stop, BUDGET),
        RunOutcome::ReachedStop
    );
    assert_eq!(direct.run(stop, BUDGET).unwrap(), RunOutcome::ReachedStop);
    equal_state(&oracle, &direct);
    assert_eq!(direct.state.regs[0], 42);
}

#[test]
fn self_modifying_store_retires_the_old_suffix_before_it_executes() {
    let code = program(|a| {
        a.mov(byte_ptr(edi), dl).unwrap();
        a.inc(eax).unwrap();
    });
    let suffix = CODE + code.len() as u32 - 1;
    let stop = CODE + code.len() as u32;
    let [mut oracle, mut direct] = pair(&code);
    for m in [&mut oracle, &mut direct] {
        m.protect(CODE, PAGE, READ | WRITE | EXEC).unwrap();
    }
    for replacement in [0x43, 0x40] {
        // Synthetic one-byte INC encodings replace the already compiled suffix.
        for m in [&mut oracle, &mut direct] {
            m.state = seed();
            m.state.regs[7] = suffix;
            m.state.regs[2] = replacement;
        }
        // Publish the currently visible bytes before each guest write, including
        // the second replacement after the first publication was invalidated.
        direct.add_region(Region::new(CODE, code.len())).unwrap();
        let entries_before = direct.stats.native_entries;
        assert_eq!(
            interpret(&mut oracle, stop, BUDGET),
            RunOutcome::ReachedStop
        );
        assert_eq!(direct.run(stop, BUDGET).unwrap(), RunOutcome::ReachedStop);
        equal_state(&oracle, &direct);
        equal_memory(&oracle, &direct, CODE, code.len());
        assert!(direct.stats.native_entries > entries_before);
        assert_eq!(
            direct.state.regs[0],
            0xaabb_ccdd + u32::from(replacement == 0x40)
        );
        assert_eq!(
            direct.state.regs[3],
            if replacement == 0x43 { 0 } else { u32::MAX }
        );
    }
    assert!(direct.stats.invalidations > 0);
}

#[test]
fn public_shared_audio_overlay_remains_coherent_at_scalar_widths() {
    use adv_cpu::backend::{Guest, Prot};

    for width in [1, 2, 4] {
        let code = program(|a| {
            store(a, width);
            load(a, width);
            a.mov(dword_ptr(esi), eax).unwrap();
        });
        let stop = CODE + code.len() as u32;
        let fixture = |native| {
            let mut cpu = JitCpu::new(stop, false).unwrap();
            configure(cpu.executor_mut(), native);
            cpu.map(CODE, PAGE as u32, Prot::RX).unwrap();
            cpu.write(CODE, &code).unwrap();
            cpu.map(DEST, PAGE as u32, Prot::RW).unwrap();
            let shared = cpu.map_shared_audio(DATA, PAGE as u32).unwrap();
            cpu.write(DATA, &[0x55; 32]).unwrap();
            let m = cpu.executor_mut();
            m.state = seed();
            m.state.regs[6] = DEST;
            m.state.regs[7] = DATA + 16;
            if native {
                m.add_region(Region::new(CODE, code.len())).unwrap();
            }
            (cpu, shared)
        };
        let (mut oracle, reference_view) = fixture(false);
        let (mut direct, view) = fixture(true);
        assert_eq!(
            interpret(oracle.executor_mut(), stop, BUDGET),
            RunOutcome::ReachedStop
        );
        assert_eq!(
            direct.executor_mut().run(stop, BUDGET).unwrap(),
            RunOutcome::ReachedStop
        );
        equal_state(oracle.executor(), direct.executor());
        equal_memory(oracle.executor(), direct.executor(), DATA, PAGE);
        equal_memory(oracle.executor(), direct.executor(), DEST, PAGE);
        let mut copied = [0; 32];
        view.read_alias.copy_into(0, &mut copied);
        assert_eq!(
            &copied[16..16 + width],
            &0x89ab_cdefu32.to_le_bytes()[..width]
        );
        assert_eq!(copied[15], 0x55);
        assert_eq!(copied[16 + width], 0x55);
        let mut expected = [0; 32];
        reference_view.read_alias.copy_into(0, &mut expected);
        assert_eq!(copied, expected);
        for cpu in [&mut oracle, &mut direct] {
            cpu.protect(DATA, PAGE as u32, Prot::READ).unwrap();
            cpu.executor_mut().state.eip = CODE;
            cpu.executor_mut().state.regs[2] = 0;
        }
        let expected = interpret(oracle.executor_mut(), stop, BUDGET);
        assert!(matches!(expected, RunOutcome::WriteFault { address, .. } if address == DATA + 16));
        assert_eq!(direct.executor_mut().run(stop, BUDGET).unwrap(), expected);
        equal_state(oracle.executor(), direct.executor());
        let mut after = [0; 32];
        view.read_alias.copy_into(0, &mut after);
        assert_eq!(after, copied);
    }
}

#[test]
fn public_file_overlays_keep_shared_and_copy_on_write_aliases_distinct() {
    use adv_cpu::backend::{FilePage, Guest, Prot};

    for width in [1, 2, 4] {
        for copy_on_write in [false, true] {
            let code = program(|a| {
                store(a, width);
                load(a, width);
                a.mov(dword_ptr(esi), eax).unwrap();
            });
            let stop = CODE + code.len() as u32;
            let fixture = |native| {
                let mut cpu = JitCpu::new(stop, false).unwrap();
                configure(cpu.executor_mut(), native);
                cpu.map(CODE, PAGE as u32, Prot::RX).unwrap();
                cpu.write(CODE, &code).unwrap();
                cpu.map(FAULT, PAGE as u32, Prot::RW).unwrap();
                let page = FilePage::new([0x55; PAGE]);
                cpu.map_file_pages(DATA, std::slice::from_ref(&page), Prot::RW, copy_on_write)
                    .unwrap();
                cpu.map_file_pages(DEST, &[page], Prot::READ, false)
                    .unwrap();
                let m = cpu.executor_mut();
                m.state = seed();
                m.state.regs[6] = FAULT;
                m.state.regs[7] = DATA + 32;
                if native {
                    m.add_region(Region::new(CODE, code.len())).unwrap();
                }
                cpu
            };
            let mut oracle = fixture(false);
            let mut direct = fixture(true);
            assert_eq!(
                interpret(oracle.executor_mut(), stop, BUDGET),
                RunOutcome::ReachedStop
            );
            assert_eq!(
                direct.executor_mut().run(stop, BUDGET).unwrap(),
                RunOutcome::ReachedStop
            );
            equal_state(oracle.executor(), direct.executor());
            for at in [DATA, DEST, FAULT] {
                equal_memory(oracle.executor(), direct.executor(), at, PAGE);
            }
            let mut alias = [0; 4];
            direct.read(DEST + 32, &mut alias[..width]).unwrap();
            let expected = if copy_on_write {
                [0x55; 4]
            } else {
                0x89ab_cdefu32.to_le_bytes()
            };
            assert_eq!(&alias[..width], &expected[..width]);
            let mask = u32::MAX >> ((4 - width) * 8);
            assert_eq!(direct.executor().state.regs[0], 0x89ab_cdef & mask);
            assert!(direct.stats().native_entries > 0);
        }
    }
}

#[test]
fn linked_region_fault_uses_the_current_frame_and_keeps_both_prefixes() {
    let caller = program(|a| {
        a.add(ebx, 1).unwrap();
        a.jmp(u64::from(NEXT)).unwrap();
    });
    let callee = assemble(NEXT, |a| {
        a.mov(dword_ptr(esi), edx).unwrap();
        a.add(ebp, 1).unwrap();
        a.mov(eax, dword_ptr(edi)).unwrap();
        a.inc(edx).unwrap();
    });
    let stop = NEXT + callee.len() as u32;
    let make = |native, linked| {
        let mut m = machine(&caller, native, CompileStrategy::Speed);
        m.map(NEXT, PAGE, READ | WRITE).unwrap();
        m.write(NEXT, &callee).unwrap();
        m.protect(NEXT, PAGE, READ | EXEC).unwrap();
        m.map(FAULT, PAGE, READ | WRITE).unwrap();
        m.write(FAULT, &42u32.to_le_bytes()).unwrap();
        m.set_region_linking(linked);
        if native {
            m.add_region(Region::new(NEXT, callee.len())).unwrap();
        }
        m
    };
    let mut oracle = make(false, false);
    let mut linked = make(true, true);
    let mut dispatched = make(true, false);
    // Strict verification runs the first four entries without links. Prime
    // both regions so the fault below really comes from a native tail chain.
    for m in [&mut linked, &mut dispatched] {
        for _ in 0..4 {
            m.state = seed();
            m.state.regs[7] = FAULT;
            assert_eq!(m.run(stop, BUDGET).unwrap(), RunOutcome::ReachedStop);
        }
        assert!(m.stats.verification.verified > 0);
    }
    for m in [&mut oracle, &mut linked, &mut dispatched] {
        m.state = seed();
        m.state.regs[7] = FAULT;
        m.protect(FAULT, PAGE, 0).unwrap();
    }
    let linked_before = linked.stats.linked_entries;
    let expected = interpret(&mut oracle, stop, BUDGET);
    assert!(matches!(
        expected,
        RunOutcome::ReadFault { address: FAULT, .. }
    ));
    assert_eq!(linked.run(stop, BUDGET).unwrap(), expected);
    assert_eq!(dispatched.run(stop, BUDGET).unwrap(), expected);
    equal_state(&oracle, &linked);
    assert_eq!(linked.state, dispatched.state);
    assert_eq!(linked.state.fuel, BUDGET - 2);
    assert_eq!(linked.state.fault_addr, FAULT);
    assert_eq!(linked.state.regs[3], 0);
    assert_eq!(linked.state.regs[5], 12);
    assert!(linked.stats.linked_entries > linked_before);
    equal_memory(&oracle, &linked, DATA, PAGE);
    for m in [&mut oracle, &mut linked, &mut dispatched] {
        m.protect(FAULT, PAGE, READ | WRITE).unwrap();
        m.write(DATA, &[0x77; 4]).unwrap();
    }
    assert_eq!(
        interpret(&mut oracle, stop, BUDGET),
        RunOutcome::ReachedStop
    );
    for m in [&mut linked, &mut dispatched] {
        assert_eq!(m.run(stop, BUDGET).unwrap(), RunOutcome::ReachedStop);
        equal_state(&oracle, m);
        assert_eq!(m.state.regs[3], 0);
        assert_eq!(m.state.regs[5], 12);
        assert_eq!(m.state.regs[2], 0x89ab_cdf0);
        assert_eq!(m.memory().read(DATA, 4).unwrap(), &[0x77; 4]);
    }
}
