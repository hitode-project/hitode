//! Focus activates the requested root and excludes disabled or foreign queues.
use super::*;
fn eligibility<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let capture = f.alloc(368);
    let proc = f.callback(true, capture, None);
    let class = f.class(proc, "focus eligibility", true);
    let a = f.create(M(class), 0, true, true);
    let b = f.create(M(class), 0, true, true);
    let child_a = geometry::create(&mut f, class, M(a), 0, [0, 0, 20, 20], true);
    let child_b = geometry::create(&mut f, class, M(b), 0, [0, 0, 20, 20], true);
    value(&mut f.code, LOG);
    f.code.mov_mem_eax(DATA);
    let previous = f.call("SetFocus", &[M(child_a)]);
    let active_a = f.call("GetActiveWindow", &[]);
    let foreground_a = f.call("GetForegroundWindow", &[]);
    let focused_a = f.call("GetFocus", &[]);
    let after_first = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(after_first);
    let switched = f.call("SetFocus", &[M(child_b)]);
    let active_b = f.call("GetActiveWindow", &[]);
    f.call("EnableWindow", &[M(b), I(0)]);
    let disabled_focus = f.call("GetFocus", &[]);
    let implicit_disabled = f.call("SetFocus", &[M(child_b)]);
    let implicit_error = f.call("kernel32.dll!GetLastError", &[]);
    f.call("EnableWindow", &[M(b), I(1)]);
    let restored = f.call("SetFocus", &[M(child_b)]);
    let bad = f.call("SetFocus", &[I(0xdeadbeef)]);
    let bad_error = f.call("kernel32.dll!GetLastError", &[]);
    let preserved = f.call("GetFocus", &[]);
    f.call("EnableWindow", &[M(child_b), I(0)]);
    let child_disabled = f.call("GetFocus", &[]);
    let blocked = f.call("SetFocus", &[M(child_b)]);
    let blocked_error = f.call("kernel32.dll!GetLastError", &[]);
    f.call("EnableWindow", &[M(child_b), I(1)]);
    f.call("SetFocus", &[M(child_b)]);
    let cleared = f.call("SetFocus", &[I(0)]);
    let empty = f.call("GetFocus", &[]);
    f.run();
    for (slot, expected) in [
        (previous, f.value(b)),
        (active_a, f.value(a)),
        (foreground_a, f.value(a)),
        (focused_a, f.value(child_a)),
        (switched, f.value(child_a)),
        (active_b, f.value(b)),
        (disabled_focus, 0),
        (implicit_disabled, 0),
        (implicit_error, 87),
        (restored, 0),
        (bad, 0),
        (bad_error, 1400),
        (preserved, f.value(child_b)),
        (child_disabled, 0),
        (blocked, 0),
        (blocked_error, 87),
        (cleared, f.value(child_b)),
        (empty, 0),
    ] {
        assert_eq!(f.value(slot), expected, "{slot:x}");
    }
    // Activating a root to focus its child sends no intermediate focus to the root.
    let focused: Vec<_> = (LOG..f.value(after_first))
        .step_by(16)
        .filter(|&p| f.value(p + 4) == wm::SETFOCUS)
        .map(|p| f.value(p))
        .collect();
    assert_eq!(focused, [f.value(child_a)]);
}
#[test]
// spec: WIN32-USER-19 WIN32-USER-20
fn focus_activates_the_requested_root_and_leaves_disabled_subtrees_until_reenabled() {
    eligibility(JitCpu::new(PARK_ADDR, false).unwrap());
    eligibility(JitCpu::new(PARK_ADDR, true).unwrap());
    eligibility(UnicornCpu::new(PARK_ADDR).unwrap());
}
fn nested<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let capture = f.alloc(368);
    let proc = f.callback(true, capture, None);
    let class = f.class(proc, "focus targets", true);
    let root = f.create(M(class), 0, true, true);
    let second = geometry::create(&mut f, class, M(root), 0, [20, 0, 20, 20], true);
    let third = geometry::create(&mut f, class, M(root), 0, [40, 0, 20, 20], true);
    let guard = f.alloc(4);
    let capture = f.alloc(368);
    let original = f.callback(true, capture, None);
    let start = f.procedure;
    let mut wrapper = Asm::new(start);
    let set = f.function("SetFocus");
    on_message(&mut wrapper, wm::KILLFOCUS, |code| {
        code.mov_eax_mem(guard);
        code.bytes.extend_from_slice(&[0x85, 0xc0]);
        skip(code, 0x84, |code| {
            value(code, 0);
            code.mov_mem_eax(guard);
            invoke(code, set, &[M(third)]);
        });
    });
    wrapper.jmp(original);
    f.machine.cpu.write(start, &wrapper.bytes).unwrap();
    f.machine.cpu.region(start, wrapper.bytes.len());
    f.procedure = wrapper.here().next_multiple_of(16);
    let guarded = f.class(start, "reentrant focus source", true);
    let first = geometry::create(&mut f, guarded, M(root), 0, [0, 0, 20, 20], true);
    f.call("SetFocus", &[M(first)]);
    value(&mut f.code, LOG);
    f.code.mov_mem_eax(DATA);
    value(&mut f.code, 1);
    f.code.mov_mem_eax(guard);
    let previous = f.call("SetFocus", &[M(second)]);
    let focus = f.call("GetFocus", &[]);
    f.run();
    assert_eq!(f.value(previous), f.value(first));
    assert_eq!(f.value(focus), f.value(third));
    let focused: Vec<_> = (LOG..f.value(DATA))
        .step_by(16)
        .filter(|&p| f.value(p + 4) == wm::SETFOCUS)
        .map(|p| f.value(p))
        .collect();
    assert_eq!(focused, [f.value(third)]);
}
#[test]
// spec: WIN32-USER-19 WIN32-USER-20
fn reentrant_focus_changes_suppress_stale_outer_setfocus_notifications() {
    nested(JitCpu::new(PARK_ADDR, false).unwrap());
    nested(JitCpu::new(PARK_ADDR, true).unwrap());
    nested(UnicornCpu::new(PARK_ADDR).unwrap());
}
