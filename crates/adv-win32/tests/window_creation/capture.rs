//! Capture notifications execute in the guest and expose the new queue state.
use super::*;

fn observed_callback<B: TestCpu>(f: &mut Fixture<B>, observed: u32, recapture: Option<u32>) -> u32 {
    let capture = f.alloc(368);
    let original = f.callback(true, capture, None);
    let start = f.procedure;
    let mut code = Asm::new(start);
    let query = f.function("GetCapture");
    let set = f.function("SetCapture");
    on_message(&mut code, wm::CAPTURECHANGED, |code| {
        invoke(code, query, &[]);
        code.mov_mem_eax(observed);
        if let Some(target) = recapture {
            invoke(code, set, &[M(target)]);
        }
    });
    code.jmp(original);
    f.machine.cpu.write(start, &code.bytes).unwrap();
    f.machine.cpu.region(start, code.bytes.len());
    f.procedure = code.here().next_multiple_of(16);
    start
}
fn notifications<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let observed = f.alloc(4);
    let proc = observed_callback(&mut f, observed, None);
    let class = f.class(proc, "capture fixture", true);
    let title = f.text("capture", true);
    let first = f.create(M(class), title, true, true);
    let capture = f.alloc(368);
    let proc = f.callback(true, capture, None);
    let other_class = f.class(proc, "other capture fixture", true);
    let second = f.create(M(other_class), title, true, true);
    let initial = f.call("SetCapture", &[M(first)]);
    let repeated = f.call("SetCapture", &[M(first)]);
    let switched = f.call("SetCapture", &[M(second)]);
    let during_switch = f.alloc(4);
    f.code.mov_eax_mem(observed).mov_mem_eax(during_switch);
    let released = f.call("ReleaseCapture", &[]);
    let empty = f.call("GetCapture", &[]);
    f.call("SetCapture", &[M(first)]);
    let invalid = f.call("SetCapture", &[I(0xdeadbeef)]);
    let error = f.call("kernel32.dll!GetLastError", &[]);
    let retained = f.call("GetCapture", &[]);
    f.call("SendMessageW", &[M(second), I(wm::CANCELMODE), I(0), I(0)]);
    let unaffected = f.call("GetCapture", &[]);
    let disabled = f.call("EnableWindow", &[M(first), I(0)]);
    let cancelled = f.call("GetCapture", &[]);
    let is_enabled = f.call("IsWindowEnabled", &[M(first)]);
    let enabled = f.call("EnableWindow", &[M(first), I(1)]);
    let is_enabled_again = f.call("IsWindowEnabled", &[M(first)]);
    f.run();
    let (a, b) = (f.value(first), f.value(second));
    for (slot, expected) in [
        (initial, 0),
        (repeated, a),
        (switched, a),
        (during_switch, b),
        (released, 1),
        (empty, 0),
        (invalid, 0),
        (error, 1400),
        (retained, a),
        (unaffected, a),
        (disabled, 0),
        (cancelled, 0),
        (is_enabled, 0),
        (enabled, 1),
        (is_enabled_again, 1),
    ] {
        assert_eq!(f.value(slot), expected, "result {slot:x}");
    }
    let messages: Vec<_> = (LOG..f.value(DATA))
        .step_by(16)
        .filter(|&at| [wm::CAPTURECHANGED, wm::CANCELMODE, wm::ENABLE].contains(&f.value(at + 4)))
        .map(|at| {
            [
                f.value(at),
                f.value(at + 4),
                f.value(at + 8),
                f.value(at + 12),
            ]
        })
        .collect();
    assert_eq!(
        messages,
        [
            [a, wm::CAPTURECHANGED, 0, b],
            [b, wm::CAPTURECHANGED, 0, 0],
            [b, wm::CANCELMODE, 0, 0],
            [a, wm::CANCELMODE, 0, 0],
            [a, wm::CAPTURECHANGED, 0, 0],
            [a, wm::ENABLE, 0, 0],
            [a, wm::ENABLE, 1, 0],
        ]
    );
}
fn reentrant<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let observed = f.alloc(4);
    let third = f.alloc(4);
    let proc = observed_callback(&mut f, observed, Some(third));
    let class = f.class(proc, "reentrant capture", true);
    let first = f.create(M(class), 0, true, true);
    let capture = f.alloc(368);
    let proc = f.callback(true, capture, None);
    let ordinary = f.class(proc, "ordinary capture", true);
    let second = f.create(M(ordinary), 0, true, true);
    let created_third = f.create(M(ordinary), 0, true, true);
    f.code.mov_eax_mem(created_third).mov_mem_eax(third);
    f.call("SetCapture", &[M(first)]);
    let old = f.call("SetCapture", &[M(second)]);
    let current = f.call("GetCapture", &[]);
    f.run();
    assert_eq!(f.value(old), f.value(first));
    assert_eq!(f.value(observed), f.value(second));
    assert_eq!(f.value(current), f.value(third));
    let changes: Vec<_> = (LOG..f.value(DATA))
        .step_by(16)
        .filter(|&at| f.value(at + 4) == wm::CAPTURECHANGED)
        .map(|at| [f.value(at), f.value(at + 12)])
        .collect();
    assert_eq!(
        changes,
        [
            [f.value(second), f.value(third)],
            [f.value(first), f.value(second)],
        ]
    );
}
#[test]
// spec: WIN32-USER-20
fn capture_switch_release_and_disable_deliver_guest_notifications() {
    notifications(JitCpu::new(PARK_ADDR, false).unwrap());
    notifications(JitCpu::new(PARK_ADDR, true).unwrap());
    notifications(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
// spec: WIN32-USER-20
fn capture_callback_can_select_a_different_owner_reentrantly() {
    reentrant(JitCpu::new(PARK_ADDR, false).unwrap());
    reentrant(JitCpu::new(PARK_ADDR, true).unwrap());
    reentrant(UnicornCpu::new(PARK_ADDR).unwrap());
}
