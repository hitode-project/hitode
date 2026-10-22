//! Queue deactivation and NC vetoes execute through authored guest procedures.
use super::*;
fn exercise<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let capture = f.alloc(368);
    let procedure = f.callback(true, capture, None);
    let class = f.class(procedure, "activation messages", true);
    let first = f.create(M(class), 0, true, true);
    let second = f.create(M(class), 0, true, true);
    value(&mut f.code, LOG);
    f.code.mov_mem_eax(DATA);
    let switched = f.call("SetActiveWindow", &[M(first)]);
    let active = f.call("GetActiveWindow", &[]);
    let focus = f.call("GetFocus", &[]);
    let cleared = f.call("SetActiveWindow", &[I(0)]);
    let inactive = f.call("GetActiveWindow", &[]);
    let unfocused = f.call("GetFocus", &[]);
    let final_log = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(final_log);
    let capture = f.alloc(368);
    let veto = f.callback(true, capture, Some((0x86, 0)));
    let veto_class = f.class(veto, "NC veto", true);
    let guarded = f.create(M(veto_class), 0, true, true);
    let rejected = f.call("SetActiveWindow", &[I(0)]);
    let retained = f.call("GetActiveWindow", &[]);
    let retained_focus = f.call("GetFocus", &[]);
    f.run();
    let (a, b) = (f.value(first), f.value(second));
    for (slot, expected) in [
        (switched, b),
        (active, a),
        (focus, a),
        (cleared, a),
        (inactive, 0),
        (unfocused, 0),
        (rejected, 0),
        (retained, f.value(guarded)),
        (retained_focus, f.value(guarded)),
    ] {
        assert_eq!(f.value(slot), expected, "{slot:x}");
    }
    let messages: Vec<_> = (LOG..f.value(final_log))
        .step_by(16)
        .map(|p| [f.value(p), f.value(p + 4), f.value(p + 8), f.value(p + 12)])
        .collect();
    assert_eq!(
        messages,
        [
            [b, 0x86, 0, 0],
            [b, wm::ACTIVATE, 0, a],
            [a, 0x86, 1, 0],
            [a, wm::ACTIVATE, 1, b],
            [b, wm::KILLFOCUS, a, 0],
            [a, wm::SETFOCUS, b, 0],
            [a, 0x86, 0, 0],
            [a, wm::ACTIVATE, 0, 0],
            [a, wm::KILLFOCUS, 0, 0],
        ]
    );
}
#[test]
// spec: WIN32-USER-19
fn activation_switch_null_exit_and_nc_veto_preserve_queue_and_focus() {
    exercise(JitCpu::new(PARK_ADDR, false).unwrap());
    exercise(JitCpu::new(PARK_ADDR, true).unwrap());
    exercise(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn foreign_queue<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let capture = f.alloc(368);
    let procedure = f.callback(true, capture, None);
    let class = f.class(procedure, "queue ownership", true);
    let first = f.create(M(class), 0, true, true);
    let ready = f.call("kernel32.dll!CreateEventW", &[I(0), I(1), I(0), I(0)]);
    let done = f.call("kernel32.dll!CreateEventW", &[I(0), I(1), I(0), I(0)]);
    let other = f.alloc(4);
    let other_active = f.alloc(4);
    let mut helper = Asm::new(CODE + 0x6000);
    invoke(
        &mut helper,
        f.function("CreateWindowExW"),
        &[
            I(0),
            M(class),
            I(0),
            I(WS_POPUP),
            I(0),
            I(0),
            I(100),
            I(100),
            I(0),
            I(0),
            I(CODE),
            I(0),
        ],
    );
    helper.mov_mem_eax(other);
    invoke(
        &mut helper,
        f.function("kernel32.dll!SetEvent"),
        &[M(ready)],
    );
    invoke(
        &mut helper,
        f.function("kernel32.dll!WaitForSingleObject"),
        &[M(done), I(u32::MAX)],
    );
    invoke(&mut helper, f.function("GetActiveWindow"), &[]);
    helper.mov_mem_eax(other_active);
    invoke(&mut helper, f.function("DestroyWindow"), &[M(other)]);
    value(&mut helper, 0);
    helper.ret_n(4);
    f.machine.cpu.write(CODE + 0x6000, &helper.bytes).unwrap();
    f.machine.cpu.region(CODE + 0x6000, helper.bytes.len());
    let thread = f.call(
        "kernel32.dll!CreateThread",
        &[I(0), I(0), I(CODE + 0x6000), I(0), I(0), I(0)],
    );
    f.call("kernel32.dll!WaitForSingleObject", &[M(ready), I(u32::MAX)]);
    let previous = f.call("SetActiveWindow", &[M(other)]);
    let active = f.call("GetActiveWindow", &[]);
    let focus = f.call("GetFocus", &[]);
    f.call("kernel32.dll!SetEvent", &[M(done)]);
    f.call(
        "kernel32.dll!WaitForSingleObject",
        &[M(thread), I(u32::MAX)],
    );
    f.run();
    assert_ne!(f.value(other), 0);
    assert_eq!(f.value(previous), f.value(first));
    assert_eq!(
        (f.value(active), f.value(focus), f.value(other_active)),
        (0, 0, 0)
    );
}
#[test]
// spec: WIN32-USER-19
fn a_foreign_queue_target_deactivates_only_the_calling_queue() {
    foreign_queue(JitCpu::new(PARK_ADDR, false).unwrap());
    foreign_queue(JitCpu::new(PARK_ADDR, true).unwrap());
    foreign_queue(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn nested<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let guard = f.alloc(4);
    let third = f.alloc(4);
    let capture = f.alloc(368);
    let original = f.callback(true, capture, None);
    let start = f.procedure;
    let mut wrapper = Asm::new(start);
    let activate = f.function("SetActiveWindow");
    on_message(&mut wrapper, wm::ACTIVATE, |code| {
        code.mov_eax_esp(12);
        code.bytes.extend_from_slice(&[0xa9, 0xff, 0xff, 0, 0]);
        skip(code, 0x85, |code| {
            code.mov_eax_mem(guard);
            code.bytes.extend_from_slice(&[0x85, 0xc0]);
            skip(code, 0x84, |code| {
                value(code, 0);
                code.mov_mem_eax(guard);
                invoke(code, activate, &[M(third)]);
            });
        });
    });
    wrapper.jmp(original);
    f.machine.cpu.write(start, &wrapper.bytes).unwrap();
    f.machine.cpu.region(start, wrapper.bytes.len());
    f.procedure = wrapper.here().next_multiple_of(16);
    let guarded = f.class(start, "nested activation", true);
    let first = f.create(M(guarded), 0, true, true);
    let capture = f.alloc(368);
    let procedure = f.callback(true, capture, None);
    let ordinary = f.class(procedure, "nested activation targets", true);
    let second = f.create(M(ordinary), 0, true, true);
    let created_third = f.create(M(ordinary), 0, true, true);
    f.code.mov_eax_mem(created_third).mov_mem_eax(third);
    f.call("SetActiveWindow", &[M(first)]);
    value(&mut f.code, LOG);
    f.code.mov_mem_eax(DATA);
    value(&mut f.code, 1);
    f.code.mov_mem_eax(guard);
    let interrupted = f.call("SetActiveWindow", &[M(second)]);
    let active = f.call("GetActiveWindow", &[]);
    let focus = f.call("GetFocus", &[]);
    f.run();
    assert_eq!(f.value(interrupted), 0);
    assert_eq!(
        (f.value(active), f.value(focus)),
        (f.value(third), f.value(third))
    );
    assert!(
        !(LOG..f.value(DATA))
            .step_by(16)
            .any(|p| f.value(p) == f.value(second)
                && f.value(p + 4) == wm::NCACTIVATE
                && f.value(p + 8) != 0)
    );
}
#[test]
// spec: WIN32-USER-19
fn a_nested_guest_activation_supersedes_pending_outer_notifications() {
    nested(JitCpu::new(PARK_ADDR, false).unwrap());
    nested(JitCpu::new(PARK_ADDR, true).unwrap());
    nested(UnicornCpu::new(PARK_ADDR).unwrap());
}
