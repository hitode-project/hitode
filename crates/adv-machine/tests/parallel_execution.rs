//! Essential lease/ABI/concurrent execution checks; no game timing assertions.
#![cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, Guest, GuestExt, Prot, Reg, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, SliceResult, layout::PARK_ADDR};
use std::time::{Duration, Instant};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;

fn machine(workers: usize) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    registry.add("test", "finish", Conv::Stdcall { argc: 0 }, |e| {
        assert_eq!(e.cpu.reg(Reg::Ebx), e.ctx().teb);
        assert_eq!(e.cpu.reg(Reg::Ecx), 0);
        e.ext.get_mut::<Vec<u32>>().push(e.context);
        Outcome::ExitThread(e.context)
    });
    registry.add("test", "stop", Conv::Stdcall { argc: 0 }, |e| {
        if e.sys.contexts.iter().any(|c| c.state_name() == "Running") {
            Outcome::ExitProcess(0)
        } else {
            Outcome::Return(0)
        }
    });
    registry.add("test", "patch", Conv::Stdcall { argc: 0 }, |e| {
        if !e.sys.contexts.iter().any(|c| c.state_name() == "Running") {
            return Outcome::Return(0);
        }
        // Mutations occur inside a real API while a native lease is outstanding,
        // not after run_slice (which itself drains all leases).
        e.cpu.protect(DATA, 4096, Prot::RW).unwrap();
        e.cpu.map(DATA + 4096, 4096, Prot::RW).unwrap();
        e.cpu.unmap(DATA + 4096, 4096).unwrap();
        e.cpu.write(CODE + 0x801, &2u32.to_le_bytes()).unwrap();
        e.cpu.write_u32(DATA, 1).unwrap();
        Outcome::Return(1)
    });
    registry.add("test", "verify_patch", Conv::Stdcall { argc: 0 }, |e| {
        assert_eq!(
            e.cpu.reg(Reg::Eax),
            2,
            "executed stale code after patch publication"
        );
        Outcome::Return(2)
    });
    registry.add("test", "callback", Conv::Stdcall { argc: 0 }, |e| {
        let context = e.context;
        Outcome::call(CODE + 0xf00, [], move |e, result| {
            assert_eq!(e.context, context);
            assert_eq!(result.value(), 7);
            assert_eq!(e.cpu.reg(Reg::Ebx), e.ctx().teb);
            let state = e.cpu.capture_architecture().unwrap();
            assert_eq!(
                u32::from_le_bytes(state.xmm[0][..4].try_into().unwrap()),
                e.ctx().teb
            );
            assert_eq!(state.mxcsr, 0x1f80);
            assert_eq!(e.cpu.pop_f64(), Some(1.0));
            Outcome::Return(0)
        })
    });
    registry.add("test", "wait", Conv::Stdcall { argc: 0 }, |e| {
        let context = e.context;
        Outcome::block(
            adv_machine::Wait::Until(e.sys.clock.now_us() + 1000),
            move |e, _| {
                assert_eq!(e.context, context);
                Outcome::Return(7)
            },
        )
    });
    let mut cpu = JitCpu::new(PARK_ADDR, true).unwrap();
    cpu.set_guest_workers(workers).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::real()).unwrap();
    machine.ext.insert(Vec::<u32>::new());
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.set_frame_handoff(false);
    machine.set_execution_budget(Some(Duration::from_secs(5)));
    machine
}

fn finish(m: &mut Machine<JitCpu>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "parallel scheduler failed to progress"
        );
        match m.run_slice(Duration::from_millis(8)) {
            SliceResult::Complete(RunResult::Exited(_)) => break,
            SliceResult::Complete(RunResult::Failed(error)) => panic!("{error:?}"),
            SliceResult::Complete(RunResult::Idle { .. }) => {
                std::thread::sleep(Duration::from_millis(1))
            }
            SliceResult::Yielded => {}
        }
    }
}

#[test]
fn two_workers_execute_three_contexts_and_deliver_each_trap_once() {
    let mut m = machine(2);
    let done = m.resolve_import("test", "finish").unwrap();
    let callback = m.resolve_import("test", "callback").unwrap();
    let wait = m.resolve_import("test", "wait").unwrap();
    let mut body = Asm::new(CODE + 0xf00);
    body.call(wait).ret();
    m.cpu.write(CODE + 0xf00, &body.bytes).unwrap();
    for id in 0..3 {
        let address = CODE + id * 256;
        let mut a = Asm::new(address);
        a.mov_eax_fs(0x18).mov_ebx_eax().mov_ecx_imm(2_000_000);
        a.bytes
            .extend_from_slice(&[0x66, 0x0f, 0x6e, 0xc3, 0xd9, 0xe8]); // movd xmm0,ebx; fld1
        let top = a.here();
        a.dec_ecx().jnz(top).call(callback).call(done).hlt();
        m.cpu.write(address, &a.bytes).unwrap();
        m.cpu
            .executor_mut()
            .add_region(Region::new(address, a.bytes.len()))
            .unwrap();
        m.spawn("compute", address, &[], 65536).unwrap();
    }
    finish(&mut m);
    let mut completed = m.ext.get::<Vec<u32>>().clone();
    completed.sort();
    assert_eq!(completed, vec![0, 1, 2]);
    let stats = m.cpu.parallel_stats();
    assert_eq!(stats.submitted, stats.completed);
    assert_eq!(stats.max_running, 2);
    assert!(stats.overlap_ns > 0);
    assert!(m.sys.contexts.iter().all(|c| c.is_done()));
}

#[test]
fn locked_fallback_and_host_memory_changes_share_one_exclusive_boundary() {
    let mut m = machine(2);
    let done = m.resolve_import("test", "finish").unwrap();
    for id in 0..2 {
        let address = CODE + id * 256;
        let mut a = Asm::new(address);
        a.mov_eax_fs(0x18).mov_ebx_eax().mov_ecx_imm(100);
        let top = a.here();
        a.bytes.extend_from_slice(&[0xf0, 0xff, 0x05]); // lock inc dword [DATA]
        a.bytes.extend_from_slice(&DATA.to_le_bytes());
        a.dec_ecx().jnz(top).call(done).hlt();
        m.cpu.write(address, &a.bytes).unwrap();
        m.cpu
            .executor_mut()
            .add_region(Region::new(address, a.bytes.len()))
            .unwrap();
        m.spawn("atomic", address, &[], 65536).unwrap();
    }
    // The same public boundary used by API mapping/patch operations must drain
    // native leases before a protected page or code allocation can change.
    let _ = m.run_slice(Duration::from_micros(100));
    m.cpu.protect(DATA, 4096, Prot::RW).unwrap();
    finish(&mut m);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 200);
    assert_eq!(m.ext.get::<Vec<u32>>().len(), 2);
    assert_eq!(m.cpu.guest_workers(), 2);
}

#[test]
fn api_patch_unmap_and_native_polling_observe_one_published_generation() {
    let mut m = machine(2);
    let patch = m.resolve_import("test", "patch").unwrap();
    let verify = m.resolve_import("test", "verify_patch").unwrap();
    let done = m.resolve_import("test", "finish").unwrap();
    let mut function = Asm::new(CODE + 0x800);
    function.bytes.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xc3]);
    let mut reader = Asm::new(CODE);
    reader.mov_eax_fs(0x18).mov_ebx_eax().mov_ecx_imm(0);
    let wait = reader.here();
    reader
        .cmp_mem_imm8(DATA, 0)
        .jz(wait)
        .call(CODE + 0x800)
        .call(verify)
        .call(done)
        .hlt();
    let mut writer = Asm::new(CODE + 0x100);
    writer.mov_eax_fs(0x18).mov_ebx_eax().mov_ecx_imm(0);
    let retry = writer.here();
    writer.call(patch);
    writer.bytes.extend_from_slice(&[0x85, 0xc0]); // test eax,eax
    writer.jz(retry).call(done).hlt();
    for (address, a) in [
        (CODE, reader),
        (CODE + 0x100, writer),
        (CODE + 0x800, function),
    ] {
        m.cpu.write(address, &a.bytes).unwrap();
        m.cpu
            .executor_mut()
            .add_region(Region::new(address, a.bytes.len()))
            .unwrap();
    }
    m.spawn("reader", CODE, &[], 65536).unwrap();
    m.spawn("writer", CODE + 0x100, &[], 65536).unwrap();
    finish(&mut m);
    assert_eq!(m.ext.get::<Vec<u32>>().len(), 2);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
    assert!(m.cpu.parallel_stats().submitted > 0);
    assert_eq!(
        m.cpu.parallel_stats().submitted,
        m.cpu.parallel_stats().completed
    );
}

#[test]
fn process_exit_drains_late_completions_without_resurrecting_running_contexts() {
    let mut m = machine(2);
    let stop = m.resolve_import("test", "stop").unwrap();
    let mut a = Asm::new(CODE);
    a.bytes.extend_from_slice(&[0xff, 0x05]); // inc dword [DATA]
    a.bytes.extend_from_slice(&DATA.to_le_bytes());
    a.jmp(CODE);
    let mut b = Asm::new(CODE + 0x100);
    b.call(stop).jmp(CODE + 0x100);
    for (pc, code) in [(CODE, a), (CODE + 0x100, b)] {
        m.cpu.write(pc, &code.bytes).unwrap();
        m.cpu
            .executor_mut()
            .add_region(Region::new(pc, code.bytes.len()))
            .unwrap();
        m.spawn("exit", pc, &[], 65536).unwrap();
    }
    finish(&mut m);
    assert!(m.sys.contexts.iter().all(|c| c.is_done()));
    let stats = m.cpu.parallel_stats();
    assert!(stats.submitted > 0);
    assert_eq!(stats.submitted, stats.completed);
    let final_value = m.cpu.read_u32(DATA).unwrap();
    std::thread::sleep(Duration::from_millis(2));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), final_value);
}

#[test]
fn unsliced_parallel_run_still_observes_a_timed_wake_during_guest_computation() {
    let mut m = machine(2);
    m.set_execution_budget(None);
    let wait = m.resolve_import("test", "wait").unwrap();
    let done = m.resolve_import("test", "finish").unwrap();
    let mut a = Asm::new(CODE);
    a.mov_eax_fs(0x18).mov_ebx_eax().mov_ecx_imm(0).call(wait)
        .mov_mem_imm(DATA, 1).call(done).hlt();
    let mut b = Asm::new(CODE + 0x100);
    b.mov_eax_fs(0x18).mov_ebx_eax().mov_ecx_imm(0);
    let spin = b.here();
    b.cmp_mem_imm8(DATA, 0).jz(spin).mov_ecx_imm(3_000_000);
    let finish_work = b.here();
    b.dec_ecx().jnz(finish_work).call(done).hlt();
    for (pc, code) in [(CODE, a), (CODE + 0x100, b)] {
        m.cpu.write(pc, &code.bytes).unwrap();
        m.cpu.executor_mut().add_region(Region::new(pc, code.bytes.len())).unwrap();
        m.spawn("timed", pc, &[], 65536).unwrap();
    }
    assert!(matches!(m.run(), RunResult::Exited(_)));
    assert_eq!(m.ext.get::<Vec<u32>>().len(), 2);
}
