//! Compare query leaves and their fallback before scheduler application.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Prot, Reg},
};
use adv_machine::{Clock, Machine, Resume, WakeReason, layout::PARK_ADDR};

const DATA: u32 = 0x410000;

fn machine() -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/scratch/hot-api-tests");
    Win32::install(&mut m, crate::Win32Config::new(&root, "queries.exe", &root)).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
    m.sys.current = m.spawn("queries", PARK_ADDR, &[], 65536).unwrap();
    let user = &mut m.ext.get_mut::<Win32>().user;
    user.keys[0x41] = 0x80;
    user.queues.entry(m.sys.current).or_default().keyboard.0[0x41] = 0x81;
    m.cpu.set_reg(Reg::Esp, DATA + 2048);
    m.cpu.set_reg(Reg::Edx, 0x12345678);
    m
}

#[derive(Debug, PartialEq, Eq)]
struct State {
    poll: Option<ClockPoll>,
    now: u64,
    error: u32,
    bytes: Vec<u8>,
}

fn state(m: &mut Machine<JitCpu>) -> State {
    let mut bytes = vec![0; 8192];
    m.cpu.read(DATA, &mut bytes).unwrap();
    let poll = m.ext.try_get::<ClockPoll>().cloned();
    let now = m.sys.clock.now_us();
    let env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    State {
        poll,
        now,
        error: crate::util::last_error(&env),
        bytes,
    }
}

fn call(
    m: &mut Machine<JitCpu>,
    module: &str,
    name: &str,
    args: &[u32],
    leaf: bool,
) -> (u64, bool, Option<u64>) {
    let entry = m.resolve_import(module, name).unwrap();
    let index = (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let binding = m.sys.thunks.get(index).f;
    for (i, arg) in args.iter().enumerate() {
        m.cpu.write_u32(DATA + 2052 + i as u32 * 4, *arg).unwrap();
    }
    let before = state(m);
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: index,
    };
    let result = leaf
        .then(|| binding.env_leaf.expect("query leaf")(&mut env))
        .flatten();
    if let Some(value) = result {
        m.sys.host_calls += 1;
        return (value, false, None);
    }
    if leaf {
        assert_eq!(state(m), before, "decline changed {name} state");
    }
    m.sys.host_calls += 1;
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: index,
    };
    let mut outcome = (binding.handler)(&mut env);
    let mut deadline = None;
    if let Outcome::Block {
        wait: Wait::Until(until),
        then,
        ..
    } = outcome
    {
        deadline = Some(until);
        env.sys.clock.advance_to(until);
        outcome = then(&mut env, Resume::Woke(WakeReason::Timeout));
    }
    let value = match outcome {
        Outcome::Return(value) => (u64::from(adv_abi::leaf_edx(&env)) << 32) | u64::from(value),
        Outcome::Fail(_) => u64::MAX,
        _ => panic!("unexpected query outcome"),
    };
    (value, leaf, deadline)
}

// spec: WIN32-API-15, A64-CTL-12
#[test]
fn query_sequences_match_returns_memory_last_error_and_poll_state() {
    let (mut leaf, mut trap) = (machine(), machine());
    for m in [&mut leaf, &mut trap] {
        m.cpu
            .write(DATA + 512, &[0x82, 0xa0, b'a', 0x82, 0])
            .unwrap();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        crate::util::set_last_error(&mut env, 0xabcdef);
    }
    for (module, name, args) in [
        (K32, "GetTickCount", vec![]),
        ("winmm.dll", "timeGetTime", vec![]),
        (K32, "QueryPerformanceCounter", vec![DATA]),
        (K32, "QueryPerformanceFrequency", vec![DATA + 8]),
        ("user32.dll", "GetKeyState", vec![0x41]),
        ("user32.dll", "GetAsyncKeyState", vec![0x41]),
        ("user32.dll", "GetKeyboardState", vec![DATA + 32]),
        (K32, "IsDBCSLeadByte", vec![0x82]),
        (K32, "IsDBCSLeadByteEx", vec![932, 0x82]),
        (K32, "IsDBCSLeadByteEx", vec![1252, 0x82]),
        ("user32.dll", "CharNextA", vec![DATA + 512]),
        ("user32.dll", "CharNextA", vec![DATA + 514]),
        ("user32.dll", "CharNextA", vec![DATA + 515]),
        ("user32.dll", "CharNextA", vec![DATA + 516]),
        (K32, "QueryPerformanceCounter", vec![DATA + 4092]),
        (K32, "QueryPerformanceFrequency", vec![0]),
        ("user32.dll", "GetKeyboardState", vec![DATA + 4000]),
        ("user32.dll", "CharNextA", vec![0]),
    ] {
        let (a, declined, wa) = call(&mut leaf, module, name, &args, true);
        assert_eq!(
            declined,
            args.first()
                .is_some_and(|p| [0, DATA + 4092, DATA + 4000].contains(p)),
            "{name}"
        );
        let (b, _, wb) = call(&mut trap, module, name, &args, false);
        assert_eq!((a, wa), (b, wb), "{name}");
        assert_eq!(state(&mut leaf), state(&mut trap), "{name}");
        assert_eq!(leaf.sys.host_calls, trap.sys.host_calls);
    }
}

// spec: WIN32-API-15, PROC-CLOCK-3, A64-CTL-12
#[test]
fn first_eligible_poll_declines_without_advancing_clock_then_blocks() {
    let (mut leaf, mut trap) = (machine(), machine());
    for i in 1..=POLL_STREAK + 2 {
        let (module, name, args) = match i % 3 {
            0 => (K32, "QueryPerformanceCounter", vec![DATA]),
            1 => (K32, "GetTickCount", vec![]),
            _ => ("winmm.dll", "timeGetTime", vec![]),
        };
        let (a, declined, wa) = call(&mut leaf, module, name, &args, true);
        let (b, _, wb) = call(&mut trap, module, name, &args, false);
        assert_eq!((a, wa), (b, wb), "read {i}");
        assert_eq!(declined, i >= POLL_STREAK, "read {i}");
        assert_eq!(wa.is_some(), i >= POLL_STREAK, "read {i}");
        assert_eq!(state(&mut leaf), state(&mut trap), "read {i}");
    }
}

// spec: WIN32-API-15, A64-CTL-12
#[test]
fn user_and_calendar_queries_match_handlers_and_decline_before_partial_writes() {
    let (mut leaf, mut trap) = (machine(), machine());
    for (module, names) in [
        ("user32.dll", crate::query_leaves::USER),
        (K32, crate::query_leaves::KERNEL),
    ] {
        for &name in names {
            let args = match name {
                "GetCursorPos"
                | "GetClipCursor"
                | "GetLocalTime"
                | "GetSystemTime"
                | "GetSystemTimeAsFileTime" => vec![DATA],
                "GetWindowRect" | "GetClientRect" | "ClientToScreen" | "ScreenToClient" => {
                    vec![leaf.ext.get::<Win32>().user.desktop, DATA]
                }
                "lstrlenA" | "lstrlenW" => vec![DATA + 512],
                "lstrcpyA" | "lstrcpyW" | "lstrcmpA" | "lstrcmpW" => vec![DATA + 768, DATA + 512],
                _ => vec![0, 0],
            };
            for m in [&mut leaf, &mut trap] {
                m.cpu.write(DATA + 512, &[b'a', 0, b'b', 0, 0, 0]).unwrap();
            }
            assert_eq!(
                call(&mut leaf, module, name, &args, true).0,
                call(&mut trap, module, name, &args, false).0,
                "{name}"
            );
            assert_eq!(state(&mut leaf), state(&mut trap), "{name}");
        }
    }
    for (module, name, args) in [
        ("user32.dll", "GetCursorPos", vec![DATA + 4092]),
        ("user32.dll", "GetClipCursor", vec![DATA + 4092]),
        ("user32.dll", "ScreenToClient", vec![0, DATA + 4092]),
        (K32, "GetLocalTime", vec![DATA + 4092]),
        (K32, "GetSystemTime", vec![0]),
        (K32, "GetSystemTimeAsFileTime", vec![u32::MAX - 3]),
    ] {
        let (a, declined, _) = call(&mut leaf, module, name, &args, true);
        assert!(declined, "{name}");
        assert_eq!(a, call(&mut trap, module, name, &args, false).0, "{name}");
        assert_eq!(state(&mut leaf), state(&mut trap), "{name}");
    }
}

// spec: WIN32-API-15
#[test]
fn string_length_leaves_return_zero_after_access_failure_and_preserve_last_error() {
    let (mut leaf, mut trap) = (machine(), machine());
    for m in [&mut leaf, &mut trap] {
        m.cpu.write(DATA + 8190, b"xy").unwrap();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        crate::util::set_last_error(&mut env, 0xabcdef);
    }
    for name in ["lstrlenA", "lstrlenW"] {
        for pointer in [0, DATA + 8190] {
            let (a, declined, _) = call(&mut leaf, K32, name, &[pointer], true);
            assert!(!declined);
            assert_eq!(a as u32, 0);
            assert_eq!(a, call(&mut trap, K32, name, &[pointer], false).0);
            assert_eq!(state(&mut leaf), state(&mut trap));
            assert_eq!(state(&mut leaf).error, 0xabcdef);
        }
    }
}
