//! Host-boundary fixtures use the same ABI registry and scheduler in every backend.
//! Eager compilation, zero interpreter entries and linking comparisons belong to x64;
//! a64 executes first visits before translating and invalidates only translated code.
#[path = "support/cpu.rs"]
mod cpu;

use adv_abi::{Conv, Registry};
use adv_cpu::Region;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::{THUNK_BASE, THUNK_STRIDE, thunk_addr};
use adv_machine::{Clock, Env, Machine, Object, Outcome, Resume, RunResult, Wait, WakeReason};
use cpu::Cpu;
use iced_x86::code_asm::*;

const CODE: u32 = 0x0040_0000;
const CALLBACK: u32 = CODE + 0x1000;
const INNER: u32 = CODE + 0x2000;
const COLD: u32 = CODE + 0x3000;
const HOOK: u32 = CODE + 0x4000;
const TRAMPOLINE: u32 = CODE + 0x5000;
const DATA: u32 = 0x0041_0000;

#[derive(Default)]
struct Calls {
    log: Vec<String>,
    event: u32,
    patch_after_deadline: bool,
    verify_pcm: Option<u32>,
}

fn leaf(env: &mut Env<'_>) -> Outcome {
    env.ext.get_mut::<Calls>().log.push("call:leaf".into());
    Outcome::Return(env.arg(0).wrapping_add(7))
}

fn callback(env: &mut Env<'_>) -> Outcome {
    if let Some(output) = env.ext.get::<Calls>().verify_pcm {
        assert_eq!(env.cpu.read_u32(output + 48 * 4).unwrap() as i32, -8);
    }
    env.ext.get_mut::<Calls>().log.push("call:callback".into());
    Outcome::call(env.arg(0), [env.arg(1)], |env, why| {
        let value = why.value();
        env.ext
            .get_mut::<Calls>()
            .log
            .push(format!("returned:{value}"));
        if env.ext.get::<Calls>().patch_after_deadline {
            let deadline = env.sys.clock.now_us() + 100;
            Outcome::block(Wait::Until(deadline), move |env, why| {
                assert_eq!(why, Resume::Woke(WakeReason::Timeout));
                env.ext.get_mut::<Calls>().log.push("deadline".into());
                env.cpu.write_u32(COLD + 1, 77).unwrap();
                Outcome::Return(value)
            })
        } else {
            Outcome::Return(value + 100)
        }
    })
}

fn event_wait(env: &mut Env<'_>) -> Outcome {
    env.ext.get_mut::<Calls>().log.push("call:wait".into());
    Outcome::block(
        Wait::Object {
            handle: env.ext.get::<Calls>().event,
            deadline: None,
        },
        |env, why| {
            assert_eq!(why, Resume::Woke(WakeReason::Signaled));
            env.ext.get_mut::<Calls>().log.push("signaled".into());
            Outcome::Return(5)
        },
    )
}

fn patch(env: &mut Env<'_>) -> Outcome {
    env.cpu.write_u32(env.arg(0), 99).unwrap();
    Outcome::Return(7)
}

fn fixture(backend: &str) -> (Machine<Cpu>, [u32; 5]) {
    let mut registry = Registry::default();
    registry.add("test", "leaf", Conv::Stdcall { argc: 1 }, leaf);
    registry.add("test", "callback", Conv::Stdcall { argc: 2 }, callback);
    registry.add("test", "cdecl", Conv::Cdecl { argc: 2 }, callback);
    registry.add("test", "wait", Conv::Stdcall { argc: 0 }, event_wait);
    registry.add("test", "patch", Conv::Stdcall { argc: 1 }, patch);
    let mut m = Machine::new(Cpu::new(backend).unwrap(), registry, Clock::virtual_time()).unwrap();
    m.cpu.map(CODE, 0x6000, Prot::RWX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let apis = ["leaf", "callback", "cdecl", "wait", "patch"]
        .map(|name| m.resolve_import("test", name).unwrap());
    let event = m.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    m.ext.insert(Calls {
        event,
        ..Calls::default()
    });
    if let Some(jit) = m.cpu.jit_mut() {
        jit.set_async_compilation(false);
        #[cfg(target_arch = "x86_64")]
        jit.executor_mut().enable_auto_jit(false);
        if backend == "jit" {
            let mut thunks = Region::new(THUNK_BASE, (m.sys.thunks.len() * THUNK_STRIDE) as usize);
            thunks
                .entries
                .extend((0..m.sys.thunks.len()).map(thunk_addr));
            jit.executor_mut().add_region(thunks).unwrap();
        }
    }
    (m, apis)
}

fn program(m: &mut Machine<Cpu>, at: u32, build: impl FnOnce(&mut CodeAssembler)) {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    let bytes = a.assemble(u64::from(at)).unwrap();
    m.cpu.write(at, &bytes).unwrap();
    if let Some(jit) = m.cpu.jit_mut() {
        // An interpreter fixture also owns a JitCpu, but never enters its regions.
        jit.executor_mut()
            .add_region(Region::new(at, bytes.len()))
            .unwrap();
    }
}

fn spawn(m: &mut Machine<Cpu>) {
    m.spawn("M", CODE, &[], 0x1_0000).unwrap();
}

fn exited(result: RunResult, value: u32) {
    assert!(
        matches!(result, RunResult::Exited(v) if v == value),
        "expected {value}, got {result:?}"
    );
}

#[test]
fn api_loop_keeps_the_shared_cpu_and_matches_unicorn() {
    let mut logs = Vec::new();
    let mut entries = [0; 2];
    for (backend, linked) in [
        ("jit", false),
        ("jit", true),
        ("interpreter", false),
        ("unicorn", false),
    ] {
        let (mut m, [leaf, ..]) = fixture(backend);
        m.cpu.set_region_linking(linked);
        program(&mut m, CODE, |a| {
            let mut again = a.create_label();
            a.mov(ecx, 1000).unwrap();
            a.xor(ebx, ebx).unwrap();
            a.set_label(&mut again).unwrap();
            a.push(ecx).unwrap();
            a.call(u64::from(leaf)).unwrap();
            a.add(ebx, eax).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(again).unwrap();
            a.mov(eax, ebx).unwrap();
            a.ret().unwrap();
        });
        spawn(&mut m);
        exited(m.run(), 507500);
        assert_eq!(m.sys.host_calls, 1000);
        if backend == "jit" {
            #[cfg(target_arch = "x86_64")]
            assert_eq!(m.cpu.stats().interpreter_entries, 0);
            entries[usize::from(linked)] = m.cpu.stats().native_entries;
        }
        logs.push(m.ext.get::<Calls>().log.clone());
    }
    assert!(logs.iter().all(|log| *log == logs[0]));
    #[cfg(target_arch = "x86_64")]
    assert!(entries[1] < entries[0]);
}

#[test]
fn nested_callbacks_can_wait_then_resume_in_the_right_order() {
    let mut logs = Vec::new();
    for backend in ["jit", "interpreter", "unicorn"] {
        let (mut m, [leaf, callback, _, wait, _]) = fixture(backend);
        program(&mut m, CODE, |a| {
            a.mov(dword_ptr(DATA as i32), esp).unwrap();
            a.push(10).unwrap();
            a.push(CALLBACK as i32).unwrap();
            a.call(u64::from(callback)).unwrap();
            a.mov(dword_ptr(DATA as i32 + 4), esp).unwrap();
            a.ret().unwrap();
        });
        program(&mut m, CALLBACK, |a| {
            a.push(dword_ptr(esp + 4)).unwrap();
            a.push(INNER as i32).unwrap();
            a.call(u64::from(callback)).unwrap();
            a.inc(eax).unwrap();
            a.ret_1(4).unwrap();
        });
        program(&mut m, INNER, |a| {
            a.call(u64::from(wait)).unwrap();
            a.add(eax, dword_ptr(esp + 4)).unwrap();
            a.push(eax).unwrap();
            a.call(u64::from(leaf)).unwrap();
            a.ret_1(4).unwrap();
        });
        spawn(&mut m);
        assert!(matches!(
            m.run(),
            RunResult::Idle {
                next_deadline: None
            }
        ));
        let paused = m.ext.get::<Calls>().log.clone();
        let wrong = m.sys.objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        });
        assert!(m.sys.set_event(wrong));
        assert!(matches!(
            m.run(),
            RunResult::Idle {
                next_deadline: None
            }
        ));
        assert_eq!(m.ext.get::<Calls>().log, paused);
        assert!(m.sys.set_event(m.ext.get::<Calls>().event));
        exited(m.run(), 223);
        assert_eq!(
            m.cpu.read_u32(DATA).unwrap(),
            m.cpu.read_u32(DATA + 4).unwrap()
        );
        #[cfg(target_arch = "x86_64")]
        if backend == "jit" {
            assert_eq!(m.cpu.stats().interpreter_entries, 0);
        }
        logs.push(m.ext.get::<Calls>().log.clone());
    }
    assert!(logs.iter().all(|log| *log == logs[0]));
    assert_eq!(
        logs[0],
        [
            "call:callback",
            "call:callback",
            "call:wait",
            "signaled",
            "call:leaf",
            "returned:22",
            "returned:123"
        ]
    );
}

#[test]
fn relocated_api_trap_in_an_inline_hook_trampoline_keeps_its_identity() {
    for backend in ["jit", "interpreter", "unicorn"] {
        let (mut m, [leaf, ..]) = fixture(backend);
        let mut thunk = [0; THUNK_STRIDE as usize];
        m.cpu.read(leaf, &mut thunk).unwrap();
        m.cpu.write(TRAMPOLINE, &thunk).unwrap();
        program(&mut m, HOOK, |a| {
            a.push(dword_ptr(esp + 4)).unwrap();
            a.call(u64::from(TRAMPOLINE)).unwrap();
            a.add(eax, 100).unwrap();
            a.ret_1(4).unwrap();
        });
        let mut jump = CodeAssembler::new(32).unwrap();
        jump.jmp(u64::from(HOOK)).unwrap();
        m.cpu
            .write(leaf, &jump.assemble(u64::from(leaf)).unwrap())
            .unwrap();
        program(&mut m, CODE, |a| {
            a.push(9).unwrap();
            a.call(u64::from(leaf)).unwrap();
            a.ret().unwrap();
        });
        if let Some(jit) = m.cpu.jit_mut() {
            jit.executor_mut()
                .add_region(Region::new(TRAMPOLINE, thunk.len()))
                .unwrap();
        }
        spawn(&mut m);
        exited(m.run(), 116);
        assert_eq!(m.ext.get::<Calls>().log, ["call:leaf"]);
        #[cfg(target_arch = "x86_64")]
        if backend == "jit" {
            assert!(m.cpu.stats().invalidations > 0);
            assert_eq!(m.cpu.stats().interpreter_entries, 0);
        }
    }
}

#[test]
fn cdecl_callback_and_deadline_resume_invalidate_warm_code() {
    for backend in ["jit", "interpreter", "unicorn"] {
        let (mut m, [_, _, callback, ..]) = fixture(backend);
        m.ext.get_mut::<Calls>().patch_after_deadline = true;
        program(&mut m, COLD, |a| {
            a.mov(eax, 1).unwrap();
            a.ret().unwrap();
        });
        program(&mut m, CALLBACK, |a| {
            a.mov(eax, dword_ptr(esp + 4)).unwrap();
            a.add(eax, 7).unwrap();
            a.ret().unwrap();
        });
        program(&mut m, CODE, |a| {
            a.call(u64::from(COLD)).unwrap();
            a.mov(dword_ptr(DATA as i32), eax).unwrap();
            a.mov(dword_ptr(DATA as i32 + 4), esp).unwrap();
            a.push(9).unwrap();
            a.push(CALLBACK as i32).unwrap();
            a.call(u64::from(callback)).unwrap();
            a.add(esp, 8).unwrap();
            a.mov(dword_ptr(DATA as i32 + 8), esp).unwrap();
            a.mov(dword_ptr(DATA as i32 + 12), eax).unwrap();
            a.call(u64::from(COLD)).unwrap();
            a.ret().unwrap();
        });
        spawn(&mut m);
        let deadline = match m.run() {
            RunResult::Idle {
                next_deadline: Some(t),
            } => t,
            result => panic!("{result:?}"),
        };
        m.sys.clock.advance_to(deadline - 1);
        assert!(matches!(m.run(), RunResult::Idle { next_deadline: Some(t) } if t == deadline));
        m.sys.clock.advance_to(deadline);
        let result = m.run();
        assert!(
            matches!(result, RunResult::Exited(77)),
            "{backend}: {result:?}"
        );
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
        assert_eq!(
            m.cpu.read_u32(DATA + 4).unwrap(),
            m.cpu.read_u32(DATA + 8).unwrap()
        );
        assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), 16);
        assert_eq!(
            m.ext.get::<Calls>().log,
            ["call:callback", "returned:16", "deadline"]
        );
        #[cfg(target_arch = "x86_64")]
        if backend == "jit" {
            assert!(m.cpu.stats().invalidations > 0);
        }
    }
}

#[test]
fn handler_code_write_exits_before_executing_stale_native_code() {
    for backend in ["jit", "interpreter", "unicorn"] {
        let (mut m, [_, _, _, _, patch]) = fixture(backend);
        // PUSH imm32; CALL rel32; MOV EAX,imm32; RET.
        program(&mut m, CODE, |a| {
            a.push((CODE + 11) as i32).unwrap();
            a.call(u64::from(patch)).unwrap();
            a.mov(eax, 1).unwrap();
            a.ret().unwrap();
        });
        spawn(&mut m);
        exited(m.run(), 99);
        assert_eq!(m.sys.host_calls, 1);
        #[cfg(target_arch = "x86_64")]
        if backend == "jit" {
            assert_eq!(m.cpu.stats().interpreter_entries, 0);
            assert!(m.cpu.stats().invalidations > 0);
        }
    }
}

#[test]
fn callback_return_trap_is_dispatched_after_cross_region_tail_calls() {
    let mut entries = [0; 2];
    for (backend, linked) in [
        ("jit", false),
        ("jit", true),
        ("interpreter", false),
        ("unicorn", false),
    ] {
        let (mut m, [_, callback, ..]) = fixture(backend);
        m.cpu.set_region_linking(linked);
        program(&mut m, COLD, |a| {
            a.inc(eax).unwrap();
            a.ret().unwrap();
        });
        program(&mut m, CALLBACK, |a| {
            a.mov(eax, dword_ptr(esp + 4)).unwrap();
            a.call(u64::from(COLD)).unwrap();
            a.ret_1(4).unwrap();
        });
        program(&mut m, CODE, |a| {
            a.push(41).unwrap();
            a.push(CALLBACK as i32).unwrap();
            a.call(u64::from(callback)).unwrap();
            a.add(eax, 7).unwrap();
            a.ret().unwrap();
        });
        spawn(&mut m);
        exited(m.run(), 149);
        assert_eq!(m.ext.get::<Calls>().log, ["call:callback", "returned:42"]);
        if backend == "jit" {
            #[cfg(target_arch = "x86_64")]
            assert_eq!(m.cpu.stats().interpreter_entries, 0);
            entries[usize::from(linked)] = m.cpu.stats().native_entries;
        }
    }
    #[cfg(target_arch = "x86_64")]
    assert!(entries[1] < entries[0]);
}

#[test]
fn floating_execution_keeps_native_regions_and_delivers_callbacks_in_order() {
    const SOURCE: u32 = DATA + 0x1000;
    const OUTPUT: u32 = DATA + 0x4000;
    const CALLBACK_OUTPUT: u32 = DATA + 0x7000;
    const SCALE: u32 = DATA + 0x9000;
    const SAMPLES: u32 = 2048;
    let source: Vec<_> = (0..SAMPLES)
        .flat_map(|i| (((i % 129) as f32 - 64.0) / 65536.0).to_le_bytes())
        .collect();
    let expected: Vec<_> = (0..SAMPLES)
        .flat_map(|i| ((((i % 129) as f64 - 64.0) / 2.0).round_ties_even() as i32).to_le_bytes())
        .collect();
    for (backend, linked) in [
        ("jit", false),
        ("jit", true),
        ("interpreter", false),
        ("unicorn", false),
    ] {
        let (mut m, [_, callback, ..]) = fixture(backend);
        m.ext.get_mut::<Calls>().verify_pcm = Some(OUTPUT);
        m.cpu.map(DATA + 4096, 0x9000, Prot::RW).unwrap();
        m.cpu.write(SOURCE, &source).unwrap();
        m.cpu.write(SCALE, &32768.0f32.to_le_bytes()).unwrap();
        m.cpu.set_region_linking(linked);
        program(&mut m, CODE, |a| {
            a.mov(ecx, SAMPLES).unwrap();
            a.mov(esi, SOURCE).unwrap();
            a.mov(edi, OUTPUT).unwrap();
            a.xor(ebx, ebx).unwrap();
            a.call(u64::from(COLD)).unwrap();
            a.mov(eax, 13).unwrap();
            a.ret().unwrap();
        });
        program(&mut m, COLD, |a| {
            let mut again = a.create_label();
            let mut skip = a.create_label();
            a.set_label(&mut again).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(ebx, eax).unwrap();
            a.fld(dword_ptr(esi)).unwrap();
            a.fmul(dword_ptr(SCALE as i32)).unwrap();
            a.fistp(dword_ptr(edi)).unwrap();
            a.cmp(ecx, 2000).unwrap();
            a.jne(skip).unwrap();
            a.push(0).unwrap();
            a.push(CALLBACK as i32).unwrap();
            a.call(u64::from(callback)).unwrap();
            a.set_label(&mut skip).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(again).unwrap();
            a.ret().unwrap();
        });
        program(&mut m, CALLBACK, |a| {
            a.push(ecx).unwrap();
            a.push(esi).unwrap();
            a.push(edi).unwrap();
            a.push(ebx).unwrap();
            a.mov(ecx, 256).unwrap();
            a.mov(esi, SOURCE).unwrap();
            a.mov(edi, CALLBACK_OUTPUT).unwrap();
            a.call(u64::from(COLD)).unwrap();
            a.pop(ebx).unwrap();
            a.pop(edi).unwrap();
            a.pop(esi).unwrap();
            a.pop(ecx).unwrap();
            a.mov(eax, 13).unwrap();
            a.ret_1(4).unwrap();
        });
        spawn(&mut m);
        exited(m.run(), 13);
        let mut output = vec![0; expected.len()];
        m.cpu.read(OUTPUT, &mut output).unwrap();
        assert_eq!(output, expected);
        let mut output = vec![0; 256 * 4];
        m.cpu.read(CALLBACK_OUTPUT, &mut output).unwrap();
        assert_eq!(output, expected[..256 * 4]);
        assert_eq!(m.ext.get::<Calls>().log, ["call:callback", "returned:13"]);
        if backend == "jit" {
            assert!(m.cpu.stats().native_entries > 0);
            #[cfg(target_arch = "x86_64")]
            assert_eq!(m.cpu.stats().interpreter_instructions, 0);
        }
    }
}
