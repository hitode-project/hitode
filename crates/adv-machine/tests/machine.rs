//! Machine-level behaviour with hand-assembled guest code: continuations, cooperative
//! contexts, blocking inside guest callbacks, thread join, traces, unimplemented imports.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Backend, Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{
    Clock, Conv, Env, Machine, Outcome, Registry, RunResult, SliceResult, Trace, Wait,
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x0040_0000;
const PROC: u32 = CODE + 0x100;
const WORKER: u32 = CODE + 0x200;
const FLAG: u32 = 0x0041_0000;

#[test]
fn cdecl_float_return_pushes_x87_without_destroying_existing_stack() {
    use adv_cpu::backend::GuestExt;
    let mut r = Registry::default();
    r.add("test", "float", Conv::Cdecl { argc: 0 }, |_| {
        Outcome::ReturnF32(1.25)
    });
    r.add("test", "done", Conv::Cdecl { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    let cpu = UnicornCpu::new(PARK_ADDR).unwrap();
    let mut m = Machine::new(cpu, r, Clock::virtual_time()).unwrap();
    m.cpu.map(CODE, 0x1000, Prot::RWX).unwrap();
    let float = m.resolve_import("test", "float").unwrap();
    let done = m.resolve_import("test", "done").unwrap();
    let mut a = Asm::new(CODE);
    a.bytes.extend_from_slice(&[0xd9, 0xe8]); // fld1
    a.call(float);
    a.bytes.extend_from_slice(&[0xde, 0xc1, 0xd9, 0x1d]); // faddp; fstp dword [CODE+0x800]
    a.bytes.extend_from_slice(&(CODE + 0x800).to_le_bytes());
    a.call(done);
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
    assert_eq!(f32::from_bits(m.cpu.read_u32(CODE + 0x800).unwrap()), 2.25);
}

#[test]
fn execution_budget_reports_running_context_pc_and_recent_calls() {
    let mut t = T::new();
    let (sleep, spawn) = (t.f("sleep"), t.f("spawn"));
    t.code(CODE, |a| {
        a.push_imm(WORKER)
            .call(spawn)
            .push_imm(1000)
            .call(sleep)
            .ret();
    });
    t.code(WORKER, |a| {
        a.bytes.extend_from_slice(&[0xeb, 0xfe]); // no trap, Sleep or other cooperative scheduling point
    });
    t.m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    t.m.set_execution_budget(Some(Duration::from_millis(30)));
    let guest_time = t.m.sys.clock.now_us();
    let start = Instant::now();
    let r = t.m.run_virtual(10_000_000);
    let RunResult::Failed(error) = r else {
        panic!("expected watchdog failure, got {r:?}")
    };
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(error.ctx, 1);
    assert_eq!(error.pc, WORKER);
    assert!(error.message.contains("execution budget exhausted"));
    assert!(error.recent.iter().any(|s| s.contains("test.dll!spawn")));
    assert!(error.recent.iter().any(|s| s.contains("test.dll!sleep")));
    assert_eq!(
        t.m.sys.clock.now_us(),
        guest_time,
        "watchdog must not advance guest time"
    );
    assert!(!t.m.sys.context(0).is_done());
    assert!(!t.m.sys.context(1).is_done());
}

#[test]
fn execution_budget_is_shared_across_cooperating_contexts() {
    let mut registry = Registry::default();
    registry.add("test", "yield", Conv::Stdcall { argc: 0 }, |e| {
        // Every individual CPU run is short; resetting the budget at a switch would
        // never expire. The guard keeps this regression test bounded even if broken.
        let calls = e.ext.get_mut::<u32>();
        *calls += 1;
        if *calls >= 100 {
            return Outcome::ExitProcess(99);
        }
        std::thread::sleep(Duration::from_millis(1));
        Outcome::block_then_return(Wait::Yield, 0)
    });
    let mut m = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.ext.insert(0u32);
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let call = m.resolve_import("test", "yield").unwrap();
    let mut a = Asm::new(CODE);
    a.call(call);
    a.bytes.extend_from_slice(&[0xeb, 0xf9]); // back to CALL
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("first", CODE, &[], 65536).unwrap();
    m.spawn("second", CODE, &[], 65536).unwrap();
    m.set_execution_budget(Some(Duration::from_millis(25)));
    let start = Instant::now();
    let result = m.run();
    assert!(
        matches!(result, RunResult::Failed(ref e) if e.message.contains("execution budget exhausted")),
        "{result:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(*m.ext.get::<u32>() < 100);
    assert!(m.sys.host_calls >= 2);
}

#[test]
fn soft_slices_preempt_pure_computation_and_preserve_round_robin_cpu_state() {
    const ITERATIONS: u32 = 2_000_000;
    fn machine() -> T {
        let mut t = T::new();
        let wait = t.f("wait");
        t.m.cpu.write_u32(FLAG + 24, 1.25f32.to_bits()).unwrap();
        t.m.cpu.write_u32(FLAG + 28, 9.5f32.to_bits()).unwrap();
        let main = t.m.spawn("M", CODE, &[], 65536).unwrap();
        let worker = t.m.spawn("W", WORKER, &[], 65536).unwrap();
        assert_eq!(main, 0);
        let handle = t.m.sys.context(worker).thread_handle;
        for (entry, counter, result, float, source, seed) in [
            (CODE, FLAG, FLAG + 8, FLAG + 16, FLAG + 24, 7u32),
            (WORKER, FLAG + 4, FLAG + 12, FLAG + 20, FLAG + 28, 19u32),
        ] {
            t.code(entry, |a| {
                a.bytes.extend_from_slice(&[0xb8]); // mov eax, seed
                a.bytes.extend_from_slice(&seed.to_le_bytes());
                a.bytes.extend_from_slice(&[0xd9, 0x05]); // fld dword [source]
                a.bytes.extend_from_slice(&source.to_le_bytes());
                a.mov_ecx_imm(ITERATIONS);
                let top = a.here();
                a.bytes.extend_from_slice(&[0xff, 0x05]); // inc dword [counter]
                a.bytes.extend_from_slice(&counter.to_le_bytes());
                a.add_eax_imm(3).dec_ecx().jnz(top).mov_mem_eax(result);
                a.bytes.extend_from_slice(&[0xd9, 0x1d]); // fstp dword [float]
                a.bytes.extend_from_slice(&float.to_le_bytes());
                if entry == CODE {
                    a.push_imm(handle).call(wait).mov_eax_mem(result);
                }
                a.ret();
            });
        }
        t.m.set_execution_budget(Some(Duration::from_secs(5)));
        t
    }
    let mut reference = machine();
    assert!(matches!(reference.m.run(), RunResult::Exited(6_000_007)));
    let mut sliced = machine();
    let guest_time = sliced.m.sys.clock.now_us();
    let started = Instant::now();
    let mut yields = 0;
    let mut both_progressed = false;
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "slices did not finish"
        );
        match sliced.m.run_slice(Duration::from_millis(2)) {
            SliceResult::Yielded => {
                yields += 1;
                both_progressed |= sliced.m.cpu.read_u32(FLAG).unwrap() > 0
                    && sliced.m.cpu.read_u32(FLAG + 4).unwrap() > 0;
            }
            SliceResult::Complete(RunResult::Exited(code)) => {
                assert_eq!(code, 6_000_007);
                break;
            }
            other => panic!("unexpected slice outcome: {other:?}"),
        }
    }
    assert!(
        yields >= 2 && both_progressed,
        "pure CPU contexts did not both advance"
    );
    for offset in [0, 4, 8, 12, 16, 20] {
        assert_eq!(
            sliced.m.cpu.read_u32(FLAG + offset).unwrap(),
            reference.m.cpu.read_u32(FLAG + offset).unwrap()
        );
    }
    assert_eq!(sliced.m.cpu.read_u32(FLAG).unwrap(), ITERATIONS);
    assert_eq!(sliced.m.cpu.read_u32(FLAG + 4).unwrap(), ITERATIONS);
    assert_eq!(
        sliced.m.sys.clock.now_us(),
        guest_time,
        "soft slices must not advance the virtual clock"
    );
}

#[test]
fn hard_watchdog_accumulates_across_soft_slices_and_sync_handlers() {
    let mut t = T::new();
    t.code(CODE, |a| {
        a.bytes.extend_from_slice(&[0xeb, 0xfe]);
    });
    t.m.spawn("M", CODE, &[], 65536).unwrap();
    t.m.set_execution_budget(Some(Duration::from_millis(25)));
    let started = Instant::now();
    let mut yielded = false;
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "soft slices renewed the hard watchdog"
        );
        match t.m.run_slice(Duration::from_millis(2)) {
            SliceResult::Yielded => yielded = true,
            SliceResult::Complete(RunResult::Failed(error)) => {
                assert!(error.message.contains("execution budget exhausted"));
                assert_eq!(error.pc, CODE);
                break;
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
    assert!(yielded);

    let mut registry = Registry::default();
    registry.add("test", "slow", Conv::Stdcall { argc: 0 }, |_| {
        std::thread::sleep(Duration::from_millis(30));
        Outcome::Return(0)
    });
    let mut m = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), registry, Clock::real()).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let call = m.resolve_import("test", "slow").unwrap();
    let mut a = Asm::new(CODE);
    a.call(call).jmp(CODE);
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("M", CODE, &[], 65536).unwrap();
    m.set_execution_budget(Some(Duration::from_millis(10)));
    assert!(
        matches!(m.run_slice(Duration::from_millis(1)), SliceResult::Complete(RunResult::Failed(ref e)) if e.message.contains("execution budget exhausted"))
    );
}

#[test]
fn soft_slices_restore_inherited_and_explicit_backend_budgets() {
    for policy in ["default", "disabled", "custom_backend"] {
        let mut t = T::new();
        assert_eq!(t.m.cpu.execution_budget(), Some(Duration::from_secs(5)));
        let expected = match policy {
            "disabled" => {
                t.m.set_execution_budget(None);
                None
            }
            "custom_backend" => {
                let custom = Some(Duration::from_millis(45));
                t.m.cpu.set_execution_budget(custom);
                custom
            }
            _ => Some(Duration::from_secs(5)),
        };
        let sleep = t.f("sleep");
        t.code(CODE, |a| {
            a.push_imm(1).call(sleep).ret();
        });
        t.m.spawn("M", CODE, &[], 65536).unwrap();
        assert!(matches!(
            t.m.run_slice(Duration::from_millis(20)),
            SliceResult::Complete(RunResult::Idle { .. })
        ));
        assert_eq!(
            t.m.cpu.execution_budget(),
            expected,
            "changed {policy} after an idle slice"
        );
    }

    let mut t = T::new();
    let exit = t.f("exit");
    t.code(CODE, |a| {
        a.cmp_mem_imm8(FLAG, 0).jz(CODE).push_imm(7).call(exit);
    });
    t.m.spawn("M", CODE, &[], 65536).unwrap();
    assert!(matches!(
        t.m.run_slice(Duration::from_millis(2)),
        SliceResult::Yielded
    ));
    t.m.cpu.write_u32(FLAG, 1).unwrap();
    assert!(matches!(t.m.run(), RunResult::Exited(7)));
    assert_eq!(
        t.m.cpu.execution_budget(),
        Some(Duration::from_secs(5)),
        "switching to run disabled the original watchdog"
    );
}

fn sleep(env: &mut Env<'_>) -> Outcome {
    if env.arg(0) == 0 {
        return Outcome::block_then_return(Wait::Yield, 0);
    }
    let until = env.sys.clock.now_us() + u64::from(env.arg(0)) * 1000;
    Outcome::block_then_return(Wait::Until(until), 0)
}

fn spawn(env: &mut Env<'_>) -> Outcome {
    let entry = env.arg(0);
    let id = env.sys.spawn(env.cpu, "W", entry, &[], 0x1_0000).unwrap();
    Outcome::Return(env.sys.context(id).thread_handle)
}

fn wait(env: &mut Env<'_>) -> Outcome {
    Outcome::block_then_return(
        Wait::Object {
            handle: env.arg(0),
            deadline: None,
        },
        0,
    )
}

/// cdecl run_proc(proc, arg): calls guest proc(arg), returns its result + 100.
fn run_proc(env: &mut Env<'_>) -> Outcome {
    let (proc, arg) = (env.arg(0), env.arg(1));
    Outcome::call(proc, [arg], |_, r| Outcome::Return(r.value() + 100))
}

/// cdecl call_twice(f): f(1) + f(2), as a chain of two guest calls.
fn call_twice(env: &mut Env<'_>) -> Outcome {
    let f = env.arg(0);
    Outcome::call(f, [1], move |_, a| {
        let a = a.value();
        Outcome::call(f, [2], move |_, b| Outcome::Return(a + b.value()))
    })
}

fn exit(env: &mut Env<'_>) -> Outcome {
    Outcome::ExitProcess(env.arg(0))
}

struct T {
    m: Machine<UnicornCpu>,
}

impl T {
    fn new() -> Self {
        let mut r = Registry::default();
        r.add("test.dll", "sleep", Conv::Stdcall { argc: 1 }, sleep);
        r.add("test.dll", "spawn", Conv::Stdcall { argc: 1 }, spawn);
        r.add("test.dll", "wait", Conv::Stdcall { argc: 1 }, wait);
        r.add_traced("test.dll", "run_proc", Conv::Cdecl { argc: 2 }, run_proc);
        r.add(
            "test.dll",
            "call_twice",
            Conv::Cdecl { argc: 1 },
            call_twice,
        );
        r.add("test.dll", "exit", Conv::Stdcall { argc: 1 }, exit);
        let cpu = UnicornCpu::new(PARK_ADDR).unwrap();
        let mut m = Machine::new(cpu, r, Clock::virtual_time()).unwrap();
        m.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
        m.cpu.map(FLAG, 0x1000, Prot::RW).unwrap();
        T { m }
    }

    fn f(&mut self, name: &str) -> u32 {
        self.m.resolve_import("TEST.DLL", name).unwrap()
    }

    fn code(&mut self, at: u32, build: impl FnOnce(&mut Asm)) {
        let mut a = Asm::new(at);
        build(&mut a);
        self.m.cpu.write(at, &a.bytes).unwrap();
    }
}

/// A guest callback run by a host call sleeps until another context sets a flag. Under
/// host→guest recursion this deadlocks. With continuations, the worker runs while the
/// callback's context is blocked.
#[test]
fn guest_callback_can_block_on_another_context() {
    let mut t = T::new();
    let (sleep, spawn, run_proc, exit) = (t.f("sleep"), t.f("spawn"), t.f("run_proc"), t.f("exit"));
    t.code(CODE, |a| {
        a.push_imm(WORKER).call(spawn);
        a.push_imm(0).push_imm(PROC).call(run_proc).add_esp(8);
        a.push_eax().call(exit);
    });
    t.code(PROC, |a| {
        let top = a.here();
        a.push_imm(1).call(sleep).cmp_mem_imm8(FLAG, 0).jz(top);
        a.mov_eax_mem(FLAG).ret();
    });
    t.code(WORKER, |a| {
        a.push_imm(5).call(sleep).mov_mem_imm(FLAG, 7).ret();
    });

    let buf = Rc::new(RefCell::new(Vec::new()));
    t.m.sys.trace = Trace::to_writer(Box::new(SharedBuf(buf.clone())));
    t.m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    let r = t.m.run_virtual(10_000_000);
    assert!(matches!(r, RunResult::Exited(107)), "{r:?}");
    assert!(
        t.m.sys.clock.now_us() >= 1_000_000 + 5_000,
        "worker's 5 ms sleep elapsed in guest time"
    );
    assert!(t.m.sys.context(1).is_done());
    let trace = String::from_utf8(buf.borrow().clone()).unwrap();
    assert_eq!(
        trace.trim_end(),
        format!("0\t0\ttest.dll!run_proc\t{PROC:x},0\t6b")
    );
}

#[test]
fn variadic_trace_marks_fixed_prefix_without_truncating_guest_arguments() {
    let mut t = T::new();
    t.m.sys.thunks.registry.add_traced(
        "test.dll",
        "variadic",
        Conv::CdeclVariadic { fixed: 3 },
        |env| Outcome::Return(env.arg(3)),
    );
    let (call, exit) = (t.f("variadic"), t.f("exit"));
    t.code(CODE, |a| {
        a.push_imm(42)
            .push_imm(3)
            .push_imm(2)
            .push_imm(1)
            .call(call)
            .add_esp(16);
        a.mov_mem_eax(FLAG);
        a.push_imm(43)
            .push_imm(6)
            .push_imm(5)
            .push_imm(4)
            .call(call)
            .add_esp(16);
        a.push_eax().call(exit);
    });
    let buf = Rc::new(RefCell::new(Vec::new()));
    t.m.sys.trace = Trace::to_writer(Box::new(SharedBuf(buf.clone())));
    t.m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    assert!(matches!(t.m.run_virtual(1_000_000), RunResult::Exited(43)));
    assert_eq!(t.m.cpu.read_u32(FLAG).unwrap(), 42);
    let trace = String::from_utf8(buf.borrow().clone()).unwrap();
    assert_eq!(trace.lines().count(), 3);
    assert!(trace.starts_with("# signature\ttest.dll!variadic\tfixed_prefix=3\tvariadic=true\n"));
    assert!(trace.contains("0\t0\ttest.dll!variadic\t1,2,3\t2a\n"));
    assert!(trace.contains("0\t0\ttest.dll!variadic\t4,5,6\t2b\n"));
}

/// Sleep(0) polling while the only other context sleeps: virtual time must still advance.
#[test]
fn yield_polling_does_not_livelock_virtual_time() {
    let mut t = T::new();
    let (sleep, spawn, exit) = (t.f("sleep"), t.f("spawn"), t.f("exit"));
    t.code(CODE, |a| {
        a.push_imm(WORKER).call(spawn);
        let top = a.here();
        a.push_imm(0).call(sleep).cmp_mem_imm8(FLAG, 0).jz(top);
        a.mov_eax_mem(FLAG).push_eax().call(exit);
    });
    t.code(WORKER, |a| {
        a.push_imm(5).call(sleep).mov_mem_imm(FLAG, 9).ret();
    });
    t.m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    let r = t.m.run_virtual(1_000_000);
    assert!(matches!(r, RunResult::Exited(9)), "{r:?}");
}

#[test]
fn chained_guest_calls() {
    let mut t = T::new();
    let (call_twice, exit) = (t.f("call_twice"), t.f("exit"));
    t.code(CODE, |a| {
        a.push_imm(PROC)
            .call(call_twice)
            .add_esp(4)
            .push_eax()
            .call(exit);
    });
    t.code(PROC, |a| {
        a.mov_eax_esp(4).add_eax_imm(10).ret();
    });
    t.m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    assert!(matches!(t.m.run_virtual(1_000_000), RunResult::Exited(23)));
}

#[test]
fn wait_on_thread_handle_joins() {
    let mut t = T::new();
    let (spawn, wait, exit) = (t.f("spawn"), t.f("wait"), t.f("exit"));
    t.code(CODE, |a| {
        a.push_imm(WORKER).call(spawn).push_eax().call(wait);
        a.mov_eax_mem(FLAG).push_eax().call(exit);
    });
    let sleep = t.f("sleep");
    t.code(WORKER, |a| {
        a.push_imm(3).call(sleep).mov_mem_imm(FLAG, 7).ret();
    });
    t.m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    assert!(matches!(t.m.run_virtual(1_000_000), RunResult::Exited(7)));
}

#[test]
fn unimplemented_import_fails_with_its_name() {
    let mut t = T::new();
    let missing = t.m.resolve_import("KERNEL32.dll", "FlsAlloc").unwrap();
    t.code(CODE, |a| {
        a.call(missing);
    });
    t.m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    let RunResult::Failed(e) = t.m.run_virtual(1_000_000) else {
        panic!()
    };
    assert!(e.message.contains("kernel32.dll!FlsAlloc"), "{e}");
}

#[test]
fn each_context_sees_its_own_teb() {
    let mut t = T::new();
    let exit = t.f("exit");
    // M exits with fs:[0x18] (NT_TIB.Self) read after W ran and switched FS away.
    let (sleep, spawn, wait) = (t.f("sleep"), t.f("spawn"), t.f("wait"));
    t.code(CODE, |a| {
        a.push_imm(WORKER).call(spawn).push_eax().call(wait);
        a.mov_eax_fs(0x18).push_eax().call(exit);
    });
    t.code(WORKER, |a| {
        a.push_imm(1).call(sleep).mov_eax_fs(0x18).ret();
    });
    t.m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    let RunResult::Exited(teb) = t.m.run_virtual(1_000_000) else {
        panic!()
    };
    assert_eq!(teb, t.m.sys.context(0).teb);
    assert_ne!(t.m.sys.context(0).teb, t.m.sys.context(1).teb);
}

struct SharedBuf(Rc<RefCell<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The detoured function's trampoline, for the host replacement to fall back on.
struct Original(u32);

#[test]
fn detour_replaces_a_guest_function_and_can_run_the_original() {
    // f(a, b) = a + b, frame-pointer style: `push ebp; mov ebp, esp; mov eax, [ebp+8]` is the
    // 6-byte prologue that moves to the trampoline.
    const F: u32 = CODE + 0x800;
    const OUT: u32 = CODE + 0xc00;
    let mut r = Registry::default();
    // The host version multiplies instead, and hands a == 7 back to the original.
    r.add("accel", "f", Conv::Cdecl { argc: 2 }, |env| {
        let (a, b) = (env.arg(0), env.arg(1));
        if a == 7 {
            let original = env.ext.get::<Original>().0;
            return Outcome::call(original, [a, b], |_, r| Outcome::Return(r.value()));
        }
        Outcome::Return(a * b)
    });
    r.add("test", "done", Conv::Cdecl { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    let mut m = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        r,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(CODE, 0x1000, Prot::RWX).unwrap();
    m.cpu
        .write(
            F,
            &[
                0x55, 0x8b, 0xec, 0x8b, 0x45, 0x08, 0x03, 0x45, 0x0c, 0x5d, 0xc3,
            ],
        )
        .unwrap();
    let done = m.resolve_import("test", "done").unwrap();
    let mut a = Asm::new(CODE);
    for (i, (x, y)) in [(3, 4), (7, 4), (5, 6)].into_iter().enumerate() {
        a.push_imm(y)
            .push_imm(x)
            .call(F)
            .add_esp(8)
            .mov_mem_eax(OUT + 4 * i as u32);
    }
    a.call(done);
    m.cpu.write(CODE, &a.bytes).unwrap();

    let d = m.detour(F, 6, "accel", "f").unwrap();
    m.ext.insert(Original(d.original));
    assert_eq!(d.entry, F);
    m.spawn("M", CODE, &[], 0x1_0000).unwrap();
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
    let got: Vec<u32> = (0..3)
        .map(|i| m.cpu.read_u32(OUT + 4 * i).unwrap())
        .collect();
    assert_eq!(
        got,
        [12, 11, 30],
        "host, original through the trampoline, host"
    );
    assert_eq!(m.sys.thunks.get(d.thunk).name, "f");
}
