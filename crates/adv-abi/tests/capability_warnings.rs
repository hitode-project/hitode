//! ABI transport fixtures only: these typed adapters deliberately reject options
//! to verify diagnostics without requiring a renderer or fabricating COM objects.
use adv_abi::{ApiDiagnostics, Env, GuestPtr, Outcome, Registry, RunMode, win32};
use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
use adv_machine::{
    Clock, Machine, Resume, WakeReason, layout::PARK_ADDR, sched::Wait, trace::Trace,
};
use adv_unicorn::UnicornCpu;
use std::{cell::RefCell, io::Write, rc::Rc};

const STACK: u32 = 0x410000;
const UNAVAILABLE: u32 = 0x8876086a;
const INVALID: u32 = 0x8876086c;
const SPECIFIC: &str = "unsupported \"mode\"\nsecond line";

#[derive(Clone, Copy)]
struct Mode {
    result: u32,
    specific: bool,
    delayed: bool,
}

#[win32(d3d9, name = "IDirect3DDevice9::TestCooperativeLevel", level = state)]
fn transport(env: &mut Env<'_>, _this: GuestPtr<()>) -> Outcome {
    let mode = *env.ext.get::<Mode>();
    if mode.delayed {
        return Outcome::Block {
            wait: Wait::Until(123),
            allow_time_skip: false,
            then: Box::new(|_, resume| {
                assert_eq!(resume, Resume::Woke(WakeReason::Timeout));
                Outcome::call_on_stack(0x400000, vec![11, 22], STACK + 0x800, |_, resume| {
                    assert_eq!(resume, Resume::Returned(0x55));
                    Outcome::defer(|_| Outcome::Return(UNAVAILABLE))
                })
            }),
        };
    }
    if mode.specific {
        adv_abi::warn_unavailable(env, "IDirect3DDevice9::TestCooperativeLevel", SPECIFIC);
    }
    Outcome::Return(mode.result)
}

#[win32(d3d9, name = "IDirect3DDevice9::SetRenderState", level = state)]
fn rejected_option(_env: &mut Env<'_>, _this: GuestPtr<()>, _state: i32, _value: u32) -> u32 {
    UNAVAILABLE
}

#[win32(d3d9, name = "IDirect3DDevice9::GetAvailableTextureMem", level = implement)]
fn unsigned_bits(_env: &mut Env<'_>, _this: GuestPtr<()>) -> u32 {
    UNAVAILABLE
}

#[win32(ole32, name = "CoInitialize", level = implement)]
fn outside_d3d(_env: &mut Env<'_>, _reserved: GuestPtr<()>) -> u32 {
    UNAVAILABLE
}

#[derive(Clone, Default)]
struct Capture(Rc<RefCell<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).unwrap()
    }
}
fn machine() -> (Machine<UnicornCpu>, Capture) {
    let mut registry = Registry::default();
    for api in [
        transport_ABI,
        rejected_option_ABI,
        unsigned_bits_ABI,
        outside_d3d_ABI,
    ] {
        registry.register(api);
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.cpu.map(STACK, 0x1000, Prot::RW).unwrap();
    machine.cpu.set_reg(Reg::Esp, STACK);
    let capture = Capture::default();
    machine.sys.trace = Trace::to_writer(Box::new(capture.clone()));
    machine.ext.insert(Mode {
        result: 0,
        specific: false,
        delayed: false,
    });
    (machine, capture)
}
fn env(machine: &mut Machine<UnicornCpu>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn arguments(env: &mut Env<'_>, caller: u32, values: &[u32]) {
    env.cpu.write_u32(STACK, caller).unwrap();
    for (index, value) in values.iter().enumerate() {
        env.cpu
            .write_u32(STACK + 4 + index as u32 * 4, *value)
            .unwrap();
    }
}

#[test]
fn hresult_only_rejections_have_bounded_stable_keys_and_do_not_become_stubs() {
    let (mut machine, capture) = machine();
    let mut env = env(&mut machine);
    arguments(&mut env, 0x12345678, &[0x1111, 99, 1]);
    for api in [transport_ABI, unsigned_bits_ABI, outside_d3d_ABI] {
        assert!(matches!(
            (api.handler)(&mut env),
            Outcome::Return(0 | UNAVAILABLE)
        ));
    }
    env.ext.get_mut::<Mode>().result = INVALID;
    assert!(matches!(
        (transport_ABI.handler)(&mut env),
        Outcome::Return(INVALID)
    ));
    assert!(env.ext.try_get::<ApiDiagnostics>().is_none());
    for index in 0..150 {
        arguments(&mut env, 0x12345678, &[0x1111 + index, 99, index]);
        assert!(matches!(
            (rejected_option_ABI.handler)(&mut env),
            Outcome::Return(UNAVAILABLE)
        ));
    }
    let diagnostics = env.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    let ((api, reason), count) = diagnostics.warnings.first_key_value().unwrap();
    assert_eq!(api, "d3d9.dll!IDirect3DDevice9::SetRenderState");
    assert!(reason.contains("HRESULT=0x8876086a"));
    assert!(reason.contains("State=0x63"));
    assert!(!reason.contains("1111"));
    assert_eq!(*count, 150);
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.gaps.is_empty());
    assert!(diagnostics.acceptance_eligible(RunMode::Strict));
    assert_eq!(capture.text().lines().count(), 1);
    assert!(capture.text().contains("# warning\t0\t0\t12345678\t"));
    for state in 0..70 {
        arguments(&mut env, 0x12345678, &[0x9999, state, 0]);
        assert!(matches!(
            (rejected_option_ABI.handler)(&mut env),
            Outcome::Return(UNAVAILABLE)
        ));
    }
    assert_eq!(env.ext.get::<ApiDiagnostics>().warnings.len(), 64);
    assert_eq!(env.ext.get::<ApiDiagnostics>().unrecorded_warnings, 7);
    assert_eq!(capture.text().lines().count(), 65);
}

#[test]
fn explicit_short_name_reason_is_kept_once_with_escaped_trace_text() {
    let (mut machine, capture) = machine();
    let mut env = env(&mut machine);
    *env.ext.get_mut::<Mode>() = Mode {
        result: UNAVAILABLE,
        specific: true,
        delayed: false,
    };
    arguments(&mut env, 0x12345678, &[0x1111]);
    for _ in 0..101 {
        assert!(matches!(
            (transport_ABI.handler)(&mut env),
            Outcome::Return(UNAVAILABLE)
        ));
    }
    let diagnostics = env.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    let ((api, reason), count) = diagnostics.warnings.first_key_value().unwrap();
    assert_eq!(api, "IDirect3DDevice9::TestCooperativeLevel");
    assert_eq!(
        reason,
        &format!("D3DERR_NOTAVAILABLE (HRESULT=0x8876086a): {SPECIFIC}")
    );
    assert_eq!(*count, 101);
    let trace = capture.text();
    assert_eq!(trace.lines().count(), 1);
    assert!(trace.contains("\\\"mode\\\"\\nsecond line"));
    assert!(!trace.contains("operation or requested option"));
}

#[test]
fn block_call_and_defer_keep_results_wait_flags_and_original_call_sample() {
    let (mut machine, capture) = machine();
    let mut env = env(&mut machine);
    env.ext.get_mut::<Mode>().delayed = true;
    arguments(&mut env, 0x12345678, &[0x1111]);
    let Outcome::Block {
        wait,
        then,
        allow_time_skip,
    } = (transport_ABI.handler)(&mut env)
    else {
        panic!("block preserved")
    };
    assert_eq!(wait, Wait::Until(123));
    assert!(!allow_time_skip);
    assert!(env.ext.try_get::<ApiDiagnostics>().is_none());
    // An unrelated call while the original scope is parked has its own state.
    *env.ext.get_mut::<Mode>() = Mode {
        result: 0,
        specific: false,
        delayed: false,
    };
    env.context = 1;
    arguments(&mut env, 0x87654321, &[0x2222]);
    assert!(matches!(
        (transport_ABI.handler)(&mut env),
        Outcome::Return(0)
    ));
    env.context = 0;
    let Outcome::Call {
        target,
        args,
        then,
        stack,
        ..
    } = then(&mut env, Resume::Woke(WakeReason::Timeout))
    else {
        panic!("call preserved")
    };
    assert_eq!(target, 0x400000);
    assert_eq!(args, [11, 22]);
    assert_eq!(stack, Some(STACK + 0x800));
    let Outcome::Defer { then } = then(&mut env, Resume::Returned(0x55)) else {
        panic!("defer preserved")
    };
    assert!(env.ext.try_get::<ApiDiagnostics>().is_none());
    assert!(matches!(then(&mut env), Outcome::Return(UNAVAILABLE)));
    let trace = capture.text();
    assert!(trace.contains("# warning\t0\t0\t12345678\t"));
    assert!(trace.ends_with("\t1111\n"));
    assert_eq!(
        env.ext
            .get::<ApiDiagnostics>()
            .warnings
            .values()
            .sum::<u64>(),
        1
    );
    assert_eq!(env.ext.get::<ApiDiagnostics>().stub_calls, 0);
}
