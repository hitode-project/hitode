#![cfg(all(feature = "pal", windows))]
use adv_jit::pal::JitCpu;
use adv_jit::{Region, RunOutcome};
use iced_x86::code_asm::*;
use pal_cpu::{Guest, Prot, SharedRegion};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
const CODE: u32 = 0x10000;
const STOP: u32 = CODE + 0x1000;
const DATA: u32 = 0x20000;
const SOURCE: u32 = 0x30000;
const STACK: u32 = 0x4ff00;

fn bytes(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(CODE as u64).unwrap()
}

fn fixture(code: &[u8], native: bool) -> (JitCpu, SharedRegion) {
    let mut cpu = JitCpu::new(STOP, false).unwrap();
    for at in [CODE, STOP] {
        cpu.map(at, 4096, Prot::RX).unwrap();
    }
    cpu.map(SOURCE, 8192, Prot::RW).unwrap();
    cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
    cpu.write(CODE, code).unwrap();
    cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
    let shared = cpu.map_shared_audio(DATA, 8192).unwrap();
    let mut machine = cpu.machine_mut();
    machine.state.eip = CODE;
    machine.state.regs[4] = STACK;
    if native {
        machine.add_region(Region::new(CODE, code.len())).unwrap();
    }
    drop(machine);
    (cpu, shared)
}

fn run(cpu: &mut JitCpu) {
    assert_eq!(
        cpu.machine_mut().run(STOP, 1_000_000).unwrap(),
        RunOutcome::ReachedStop
    );
}

#[test]
fn alias_survives_protect_partial_unmap_address_reuse_and_machine_destruction() {
    let (mut cpu, old) = fixture(&[0xc3], false);
    assert_eq!(old.guest_base, DATA);
    assert_eq!(old.len, 8192);
    cpu.write(DATA, &[0x12, 0x34]).unwrap();
    cpu.write(DATA + 4096, &[0x56]).unwrap();
    assert!(cpu.machine().memory().read(DATA, 1).is_err());
    cpu.protect(DATA, 8192, Prot(0)).unwrap();
    assert!(!cpu.accessible(DATA, 1, Prot::READ));
    assert_eq!(old.read_alias.load_u8(0), 0x12);
    cpu.protect(DATA, 8192, Prot::RW).unwrap();
    cpu.write(DATA + 1, &[0x91]).unwrap();
    cpu.unmap(DATA + 4096, 4096).unwrap();
    assert!(cpu.read(DATA + 4096, &mut [0; 1]).is_err());
    cpu.map(DATA + 4096, 4096, Prot::RW).unwrap();
    cpu.write(DATA + 4096, &[0xa9]).unwrap();
    assert_eq!(old.read_alias.load_u8(4096), 0x56);
    cpu.unmap(DATA, 4096).unwrap();
    let new = cpu.map_shared_audio(DATA, 4096).unwrap();
    cpu.write(DATA, &[0xaa]).unwrap();
    assert_eq!(new.read_alias.load_u8(0), 0xaa);
    assert_eq!(old.read_alias.load_u8(0), 0x12);
    assert_eq!(old.read_alias.load_u8(1), 0x91);
    drop(cpu);
    assert_eq!(new.read_alias.load_u8(0), 0xaa);
    assert_eq!(old.read_alias.load_u8(4096), 0x56);
}

#[test]
fn native_integer_mmx_sse_and_x87_accesses_exit_before_raw_memory() {
    let code = bytes(|a| {
        a.mov(dword_ptr(DATA as i32), 0x1234_5678).unwrap();
        a.mov(eax, dword_ptr(DATA as i32)).unwrap();
        a.movq(qword_ptr(DATA as i32 + 8), mm0).unwrap();
        a.movq(mm1, qword_ptr(DATA as i32 + 8)).unwrap();
        a.movdqu(ptr(DATA as i32 + 32), xmm0).unwrap();
        a.movdqu(xmm1, ptr(DATA as i32 + 32)).unwrap();
        a.emms().unwrap();
        a.fld1().unwrap();
        a.fstp(qword_ptr(DATA as i32 + 64)).unwrap();
        a.ret().unwrap();
    });
    for native in [false, true] {
        let (mut cpu, shared) = fixture(&code, native);
        cpu.machine_mut().state.mmx[0] = 0x8123_4567_89ab_cdef;
        cpu.machine_mut().state.mmx_mode = 1;
        cpu.machine_mut().state.xmm[0] = [0xa5; 16];
        run(&mut cpu);
        let machine = cpu.machine();
        assert_eq!(machine.state.regs[0], 0x1234_5678);
        assert_eq!(machine.state.mmx[1], 0x8123_4567_89ab_cdef);
        assert_eq!(machine.state.xmm[1], [0xa5; 16]);
        assert!(machine.stats.interpreter_instructions >= 7);
        let mut data = [0; 72];
        shared.read_alias.copy_into(0, &mut data);
        assert_eq!(&data[..4], &0x1234_5678u32.to_le_bytes());
        assert_eq!(&data[8..16], &0x8123_4567_89ab_cdefu64.to_le_bytes());
        assert_eq!(&data[32..48], &[0xa5; 16]);
        assert_eq!(&data[64..72], &1.0f64.to_le_bytes());
    }
}

#[test]
fn native_rep_copy_fill_and_comparison_use_byte_atomic_storage() {
    let copy = bytes(|a| {
        a.rep().movsd().unwrap();
        a.ret().unwrap();
    });
    let fill = bytes(|a| {
        a.rep().stosd().unwrap();
        a.ret().unwrap();
    });
    let compare = bytes(|a| {
        a.repe().cmpsb().unwrap();
        a.ret().unwrap();
    });
    for (code, kind) in [(&copy, 0), (&fill, 1), (&compare, 2)] {
        let (mut cpu, shared) = fixture(code, true);
        let source: Vec<_> = (0..8192).map(|i| (i % 251) as u8).collect();
        cpu.write(SOURCE, &source).unwrap();
        cpu.machine_mut().state.regs[0] = 0x7654_3210;
        cpu.machine_mut().state.regs[1] = if kind == 2 { 8192 } else { 2048 };
        cpu.machine_mut().state.regs[6] = SOURCE;
        cpu.machine_mut().state.regs[7] = DATA;
        if kind == 2 {
            cpu.write(DATA, &source).unwrap();
        }
        run(&mut cpu);
        assert_eq!(cpu.machine().state.regs[1], 0);
        let mut actual = vec![0; 8192];
        shared.read_alias.copy_into(0, &mut actual);
        if kind == 1 {
            assert_eq!(actual, 0x7654_3210u32.to_le_bytes().repeat(2048));
        } else {
            assert_eq!(actual, source);
        }
    }
}

#[test]
fn api_overlap_copy_and_mixed_page_stores_keep_their_exact_bytes() {
    let (mut cpu, shared) = fixture(&[0xc3], false);
    let source: Vec<_> = (0..8192).map(|i| (i % 251) as u8).collect();
    cpu.write(DATA, &source).unwrap();
    cpu.copy_memory(DATA, DATA + 3, 7000).unwrap();
    let mut expected = source.clone();
    expected.copy_within(0..7000, 3);
    let mut actual = vec![0; 8192];
    shared.read_alias.copy_into(0, &mut actual);
    assert_eq!(actual, expected);
    cpu.map(DATA - 4096, 4096, Prot::RW).unwrap();
    cpu.write(DATA - 2, &[9, 8, 7, 6]).unwrap();
    let mut mixed = [0; 4];
    cpu.read(DATA - 2, &mut mixed).unwrap();
    assert_eq!(mixed, [9, 8, 7, 6]);
    assert_eq!(shared.read_alias.load_u8(0), 7);
}

#[test]
fn protected_cross_page_store_faults_without_partial_commit_then_restarts() {
    let code = bytes(|a| {
        a.mov(dword_ptr(DATA as i32 + 4094), 0x1234_5678).unwrap();
        a.ret().unwrap();
    });
    let (mut cpu, shared) = fixture(&code, true);
    cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(
        cpu.machine_mut().run(STOP, 100).unwrap(),
        RunOutcome::WriteFault {
            pc: CODE,
            address: DATA + 4094
        }
    );
    assert_eq!(shared.read_alias.load_u8(4094), 0);
    assert_eq!(shared.read_alias.load_u8(4095), 0);
    cpu.protect(DATA + 4096, 4096, Prot::RW).unwrap();
    run(&mut cpu);
    let mut actual = [0; 4];
    shared.read_alias.copy_into(4094, &mut actual);
    assert_eq!(actual, 0x1234_5678u32.to_le_bytes());
}

#[test]
fn concurrent_callback_reads_never_observe_invalidated_or_reused_guest_storage() {
    let (mut cpu, shared) = fixture(&[0xc3], false);
    cpu.write(DATA, &[0x11; 8192]).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(std::sync::Barrier::new(2));
    let reader_ready = ready.clone();
    let stop = done.clone();
    let alias = shared.read_alias.clone();
    let reader = std::thread::spawn(move || {
        let mut loops = 0;
        let mut bytes = [0; 128];
        reader_ready.wait();
        while !stop.load(Ordering::Acquire) {
            alias.copy_into(loops % 8000, &mut bytes);
            assert!(bytes.iter().all(|&b| b == 0x11 || b == 0xee));
            loops += 1;
        }
        loops
    });
    ready.wait();
    for _ in 0..64 {
        cpu.write(DATA, &[0xee; 8192]).unwrap();
        cpu.write(DATA, &[0x11; 8192]).unwrap();
    }
    cpu.unmap(DATA, 8192).unwrap();
    cpu.map(DATA, 8192, Prot::RW).unwrap();
    cpu.write(DATA, &[0xa5; 8192]).unwrap();
    drop(cpu);
    done.store(true, Ordering::Release);
    assert!(reader.join().unwrap() > 0);
    assert_eq!(shared.read_alias.load_u8(0), 0x11);
}
