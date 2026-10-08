//! Self-authored production lease and dispatch differential fixtures.
use super::{execution::Program, lift, lower_cl as lower};
use crate::{
    CpuState, EXEC, Executor, Memory, READ, Region, RunOutcome, WRITE,
    interpreter::{Interpreter, Step},
    memory::gir::LeaseCache,
};
use iced_x86::{Decoder, DecoderOptions, code_asm::*};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const BASE: u32 = 0x10000;
const DATA: u32 = BASE + 0x2000;
const STOP: u32 = BASE + 0x6000;
const LEN: usize = 0x8000;

fn assemble(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(u64::from(BASE)).unwrap()
}
fn decoded(bytes: &[u8]) -> BTreeMap<u32, iced_x86::Instruction> {
    Decoder::with_ip(32, bytes, u64::from(BASE), DecoderOptions::NONE)
        .into_iter()
        .map(|i| (i.ip32(), i))
        .collect()
}
fn memory(bytes: &[u8]) -> Memory {
    let mut m = Memory::reserve(BASE, LEN).unwrap();
    m.map(BASE, LEN, READ | WRITE).unwrap();
    m.write(BASE, bytes).unwrap();
    m.protect(BASE, 4096, READ | EXEC).unwrap();
    m.watch_code(BASE, bytes.len());
    m
}
fn cpu() -> CpuState {
    CpuState {
        eip: BASE,
        eflags: 0x202 | 0x811,
        regs: [7, 3, 0, 0, DATA + 4096, 0, DATA, 0],
        ..CpuState::default()
    }
}
fn program(bytes: &[u8], m: &mut Memory) -> Program {
    let pages = vec![(0, m.versions[0])];
    Program::compile(&decoded(bytes), BASE, m, pages).unwrap()
}
fn run(
    p: &Program,
    m: &mut Memory,
    cache: &mut LeaseCache,
    budget: u64,
) -> (lower::Frame, u32, bool) {
    let mut frame = lower::Frame::from_cpu(&cpu());
    let (status, proven) = p
        .run(
            &mut frame,
            m,
            cache,
            budget,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            STOP,
        )
        .unwrap();
    (frame, status, proven)
}
fn oracle_prefix(bytes: &[u8], frame: &lower::Frame, candidate: &Memory) {
    let mut m = memory(bytes);
    let mut state = cpu();
    let mut oracle = Interpreter::new();
    for _ in 0..frame.committed {
        assert_eq!(oracle.step(&mut state, &mut m), Step::Executed);
    }
    assert_eq!(frame.gpr, state.regs);
    assert_eq!(frame.flags, state.eflags);
    assert_eq!(frame.pc, state.eip);
    assert_eq!(
        candidate.read_data(BASE, LEN).unwrap(),
        m.read_data(BASE, LEN).unwrap()
    );
}

#[test]
fn gir_proven_and_checked_mapping_permission_and_code_epochs() {
    let bytes = assemble(|a| {
        a.inc(ebx).unwrap();
        a.mov(dword_ptr(DATA as i32), eax).unwrap();
        a.inc(eax).unwrap();
    });
    let mut m = memory(&bytes);
    let p = program(&bytes, &mut m);
    let mut cache = LeaseCache::default();
    let (frame, status, proven) = run(&p, &mut m, &mut cache, 10);
    assert!(proven);
    assert_eq!(status, lower::COMPLETE);
    assert_eq!(frame.committed, 3);
    oracle_prefix(&bytes, &frame, &m);
    let epoch = frame.lease_epoch;
    m.protect(DATA, 4096, READ).unwrap();
    let (frame, status, proven) = run(&p, &mut m, &mut cache, 10);
    assert!(!proven);
    assert!(frame.lease_epoch > epoch);
    assert_eq!(status, lower::WRITE_CHECK);
    assert_eq!(frame.committed, 1);
    assert_eq!(frame.address, DATA);
    assert_eq!(frame.gpr[3], 1);
    assert_eq!(frame.gpr[0], 7);
    let mut state = cpu();
    frame.restore(&mut state);
    assert_eq!(
        Interpreter::new().step(&mut state, &mut m),
        Step::WriteFault {
            pc: frame.pc,
            address: DATA
        }
    );
    m.unmap(DATA, 4096).unwrap();
    assert_eq!(run(&p, &mut m, &mut cache, 10).1, lower::WRITE_CHECK);
    m.map(DATA, 4096, WRITE).unwrap();
    let (frame, status, proven) = run(&p, &mut m, &mut cache, 10);
    assert!(proven);
    assert_eq!(status, lower::COMPLETE);
    assert_eq!(frame.committed, 3);
    m.write(BASE, &[0x90]).unwrap();
    let (frame, status, _) = run(&p, &mut m, &mut cache, 10);
    assert_eq!(status, lower::REVOKED);
    assert_eq!(frame.committed, 0);
    assert_eq!(frame.pc, BASE);
}

#[test]
fn gir_failed_entry_proof_does_not_deliver_unreached_fault() {
    let bytes = assemble(|a| {
        let mut done = a.create_label();
        a.test(eax, eax).unwrap();
        a.jnz(done).unwrap();
        a.mov(eax, dword_ptr(DATA as i32)).unwrap();
        a.set_label(&mut done).unwrap();
        a.inc(ebx).unwrap();
    });
    let mut m = memory(&bytes);
    m.protect(DATA, 4096, 0).unwrap();
    let p = program(&bytes, &mut m);
    let (frame, status, proven) = run(&p, &mut m, &mut LeaseCache::default(), 10);
    assert!(!proven);
    assert_eq!(status, lower::COMPLETE);
    assert_eq!(frame.committed, 3);
    assert_eq!(frame.gpr[3], 1);
}

#[test]
fn gir_cross_page_rmw_failure_keeps_flags_memory_and_fault_continuation() {
    let bytes = assemble(|a| {
        a.inc(ebx).unwrap();
        a.add(dword_ptr((DATA + 4094) as i32), eax).unwrap();
    });
    let mut m = memory(&bytes);
    let p = program(&bytes, &mut m);
    m.write(DATA + 4094, &[1, 2, 3, 4]).unwrap();
    m.protect(DATA + 4096, 4096, READ).unwrap();
    let (frame, status, proven) = run(&p, &mut m, &mut LeaseCache::default(), 10);
    assert!(!proven);
    assert_eq!(status, lower::WRITE_CHECK);
    assert_eq!(frame.committed, 1);
    let mut expected = cpu();
    let mut reference = memory(&bytes);
    reference.write(DATA + 4094, &[1, 2, 3, 4]).unwrap();
    reference.protect(DATA + 4096, 4096, READ).unwrap();
    let mut oracle = Interpreter::new();
    assert_eq!(oracle.step(&mut expected, &mut reference), Step::Executed);
    assert_eq!(frame.gpr, expected.regs);
    assert_eq!(frame.flags, expected.eflags);
    let mut resumed = cpu();
    frame.restore(&mut resumed);
    assert_eq!(
        Interpreter::new().step(&mut resumed, &mut m),
        oracle.step(&mut expected, &mut reference)
    );
    assert_eq!(m.read_data(DATA + 4094, 4).unwrap().as_ref(), [1, 2, 3, 4]);
    m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
    assert_eq!(
        Interpreter::new().step(&mut resumed, &mut m),
        Step::Executed
    );
    assert_eq!(resumed.regs[3], 1);
}

#[test]
fn gir_lease_excludes_shared_lazy_cow_and_resident_file_backing() {
    let mut cache = LeaseCache::default();
    let mut memory = Memory::reserve(BASE, LEN).unwrap();
    memory.map_shared_audio(DATA, 4096).unwrap();
    assert!(!cache.acquire(&mut memory).unwrap().allows(DATA, 4, false));
    memory.unmap(DATA, 4096).unwrap();
    for cow in [false, true] {
        let page = crate::backend::FilePage::lazy(|| Ok([3; 4096]));
        memory
            .map_file_pages(DATA, &[page], READ | WRITE, cow)
            .unwrap();
        assert!(!cache.acquire(&mut memory).unwrap().allows(DATA, 4, true));
        // Initialize/promote through the ordinary accessor. A resident file
        // page still has external aliases and cannot enter a GIR lease.
        let mut value = [0; 4];
        memory.read_into(DATA, &mut value).unwrap();
        assert!(!cache.acquire(&mut memory).unwrap().allows(DATA, 4, false));
        memory.unmap(DATA, 4096).unwrap();
    }
}

#[test]
fn gir_checkpoint_layout_and_entry_modes_are_explicit() {
    use std::mem::{offset_of, size_of};
    assert_eq!(offset_of!(lower::Frame, committed), 40);
    assert_eq!(offset_of!(lower::Frame, lease_epoch), 56);
    assert_eq!(offset_of!(lower::Frame, last_indirect_branch), 64);
    assert_eq!(size_of::<lower::Frame>(), 80);
    let mut state = cpu();
    assert!(super::execution::Engine::mode_allowed(&state));
    state.eflags |= 1 << 8;
    assert!(!super::execution::Engine::mode_allowed(&state));
    state.eflags &= !(1 << 8);
    state.x87.status |= 0x80;
    assert!(!super::execution::Engine::mode_allowed(&state));
}

#[test]
fn gir_interrupt_and_revocation_poll_restore_exact_loop_prefix() {
    let bytes = assemble(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.add(eax, 3).unwrap();
        a.mov(dword_ptr(DATA as i32), eax).unwrap();
        a.jmp(top).unwrap();
    });
    let r = lift::lift(&decoded(&bytes), BASE).unwrap().region;
    let code = lower::compile_proven(&r, BASE, LEN).unwrap();
    for revoke in [false, true] {
        let flag = Arc::new(AtomicBool::new(false));
        let notify = flag.clone();
        let mut m = memory(&bytes);
        let mut frame = lower::Frame::from_cpu(&cpu());
        let mut cache = LeaseCache::default();
        let mut lease = cache.acquire(&mut m).unwrap();
        let inactive = AtomicBool::new(false);
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1));
            notify.store(true, Ordering::Release);
        });
        let status = code
            .run_lease(
                &mut frame,
                &mut lease,
                100_000_000,
                if revoke { &inactive } else { &flag },
                if revoke { &flag } else { &inactive },
                u64::from(STOP),
            )
            .unwrap();
        producer.join().unwrap();
        drop(lease);
        assert_eq!(
            status,
            if revoke {
                lower::REVOKED
            } else {
                lower::INTERRUPTED
            }
        );
        oracle_prefix(&bytes, &frame, &m);
        // An already pending request executes no additional memory or state.
        let old = frame.clone();
        let mut lease = cache.acquire(&mut m).unwrap();
        code.run_lease(
            &mut frame,
            &mut lease,
            20,
            &flag,
            &inactive,
            u64::from(STOP),
        )
        .unwrap();
        assert_eq!(frame.committed, old.committed);
        assert_eq!(frame.gpr, old.gpr);
        assert_eq!(frame.pc, old.pc);
    }
    assert_eq!(code.table.version, 1);
    assert!(!code.table.returns.is_empty());
    assert!(
        code.table
            .returns
            .iter()
            .all(|(offset, _)| (*offset as usize) < code.machine.len())
    );
    assert_eq!(code.table.snapshots, r.snapshots);
}

fn executor(bytes: &[u8], gir: bool, strict: bool) -> Executor {
    let mut m = Executor::new(BASE, LEN).unwrap();
    m.set_gir_state_ssa(gir).unwrap();
    m.set_tier_verification(strict.then_some(true));
    m.protect(BASE, LEN, READ | WRITE).unwrap();
    m.write(BASE, bytes).unwrap();
    m.protect(BASE, 4096, READ | WRITE | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.state = cpu();
    if gir {
        m.add_region(Region::new(BASE, bytes.len())).unwrap();
    }
    m
}

#[test]
fn gir_selector_strict_budget_smc_and_fallback_match_interpreter() {
    let bytes = assemble(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.add(eax, 3).unwrap();
        a.mov(dword_ptr(DATA as i32), eax).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.jmp(u64::from(STOP)).unwrap();
    });
    for strict in [false, true] {
        for budget in 1..=14 {
            let mut candidate = executor(&bytes, true, strict);
            let mut oracle = executor(&bytes, false, false);
            assert_eq!(
                candidate.run(STOP, budget).unwrap(),
                oracle.run(STOP, budget).unwrap()
            );
            assert_eq!(candidate.state, oracle.state);
            assert_eq!(
                candidate.memory().read_data(DATA, 4096).unwrap(),
                oracle.memory().read_data(DATA, 4096).unwrap()
            );
            assert_eq!(candidate.gir_stats().units, budget.min(13));
            if strict {
                assert!(candidate.stats.verification.verified > 0);
                assert_eq!(candidate.stats.verification.inconclusive, 0);
                assert_eq!(candidate.stats.verification.mismatches, 0);
            }
        }
    }
    let bytes = assemble(|a| {
        a.inc(ebx).unwrap();
        a.mov(byte_ptr((BASE + 0x100) as i32), 0x43).unwrap();
        a.jmp(u64::from(BASE + 0x100)).unwrap();
    });
    let mut m = executor(&bytes, true, true);
    let mut a = CodeAssembler::new(32).unwrap();
    a.inc(eax).unwrap();
    a.jmp(u64::from(STOP)).unwrap();
    m.write(BASE + 0x100, &a.assemble(u64::from(BASE + 0x100)).unwrap())
        .unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[3], 2);
    assert_eq!(m.state.regs[0], 7);
    assert!(m.gir_stats().cold > 0);
    assert!(m.gir_stats().invalidations > 0);
    assert_eq!(m.stats.verification.mismatches, 0);
}

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn gir_direct_checked_alias_survives_mixed_host_page_permissions() {
    let bytes = assemble(|a| {
        a.mov(dword_ptr(DATA as i32), eax).unwrap();
        a.mov(ebx, dword_ptr(DATA as i32)).unwrap();
    });
    let mut m = memory(&bytes);
    m.direct_test_pages(16384).unwrap();
    m.protect(DATA + 4096, 4096, 0).unwrap();
    let p = program(&bytes, &mut m);
    let (frame, status, proven) = run(&p, &mut m, &mut LeaseCache::default(), 10);
    assert!(proven);
    assert_eq!(status, lower::COMPLETE);
    assert_eq!(frame.gpr[3], 7);
}
