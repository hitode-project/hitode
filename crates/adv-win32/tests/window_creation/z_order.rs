//! Guest topmost entry/exit and owner propagation preserve foreground independently.
use super::*;
fn position<B: TestCpu>(f: &mut Fixture<B>, hwnd: u32, after: u32) -> u32 {
    f.call(
        "SetWindowPos",
        &[M(hwnd), I(after), I(0), I(0), I(0), I(0), I(0x13)],
    )
}
fn extended<B: TestCpu>(f: &mut Fixture<B>, hwnd: u32) -> u32 {
    f.call("GetWindowLongW", &[M(hwnd), I((-20i32) as u32)])
}
fn ordering<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    f.machine
        .sys
        .thunks
        .resolver_mut::<Registry>()
        .unwrap()
        .add(
            "<zorder-fixture>",
            "activation",
            Conv::Stdcall { argc: 1 },
            |env| {
                let active = env.arg(0);
                adv_machine::Outcome::Return(u32::from(adv_win32::user32::host_input::post(
                    env,
                    wm::ACTIVATEAPP,
                    active,
                    0,
                )))
            },
        );
    let capture = f.alloc(368);
    let proc = f.callback(true, capture, None);
    let class = f.class(proc, "z-order states", true);
    let a = geometry::create(&mut f, class, I(0), 0, [10, 10, 100, 100], false);
    f.call("ShowWindow", &[M(a), I(5)]);
    let b = geometry::create(&mut f, class, I(0), 8, [20, 20, 100, 100], false);
    f.call("ShowWindow", &[M(b), I(8)]);
    let c = geometry::create(&mut f, class, I(0), 0, [30, 30, 100, 100], false);
    f.call("ShowWindow", &[M(c), I(8)]);
    let top = f.call("GetTopWindow", &[I(0)]);
    let next = f.call("GetWindow", &[M(b), I(2)]);
    let foreground = f.call("GetForegroundWindow", &[]);
    let child_a = geometry::create(&mut f, class, M(a), 0, [1, 1, 50, 50], true);
    let child_b = geometry::create(&mut f, class, M(a), 0, [1, 1, 50, 50], true);
    let brought_child = f.call("BringWindowToTop", &[M(child_b)]);
    let child_hit = f.call("ChildWindowFromPoint", &[M(a), I(5), I(5)]);
    let after_child = f.call("GetTopWindow", &[I(0)]);
    let raise_start = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(raise_start);
    let raised = position(&mut f, a, u32::MAX);
    let raise_end = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(raise_end);
    let top_after_raise = f.call("GetTopWindow", &[I(0)]);
    let a_top = extended(&mut f, a);
    let d = geometry::create(&mut f, class, M(a), 0, [40, 40, 50, 50], false);
    let inherited = extended(&mut f, d);
    let owned_top = f.call("GetTopWindow", &[I(0)]);
    let lower_start = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(lower_start);
    let lowered = position(&mut f, a, u32::MAX - 1);
    let lower_end = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(lower_end);
    let a_normal = extended(&mut f, a);
    let d_normal = extended(&mut f, d);
    let top_after_lower = f.call("GetTopWindow", &[I(0)]);
    let owned_promoted = position(&mut f, d, u32::MAX);
    let owner_unchanged = extended(&mut f, a);
    let d_top = extended(&mut f, d);
    let owned_lowered = position(&mut f, d, u32::MAX - 1);
    let d_lowered = extended(&mut f, d);
    let bottomed = position(&mut f, b, 1);
    let b_normal = extended(&mut f, b);
    let bottom_top = f.call("GetTopWindow", &[I(0)]);
    let brought = f.call("BringWindowToTop", &[M(c)]);
    let brought_top = f.call("GetTopWindow", &[I(0)]);
    let brought_foreground = f.call("GetForegroundWindow", &[]);
    let brought_active = f.call("GetActiveWindow", &[]);
    f.call("<zorder-fixture>!activation", &[I(0)]);
    let inactive = f.call("GetForegroundWindow", &[]);
    f.call("<zorder-fixture>!activation", &[I(1)]);
    let active_again = f.call("GetForegroundWindow", &[]);
    let no_send_start = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(no_send_start);
    let no_send = f.call(
        "SetWindowPos",
        &[M(c), I(0xdeadbeef), I(0), I(0), I(0), I(0), I(0x417)],
    );
    let no_send_end = f.alloc(4);
    f.code.mov_eax_mem(DATA).mov_mem_eax(no_send_end);
    let invalid = f.call("BringWindowToTop", &[I(0xdeadbeef)]);
    let error = f.call("kernel32.dll!GetLastError", &[]);
    f.run();
    let (a, b, c, d) = (f.value(a), f.value(b), f.value(c), f.value(d));
    for (slot, expected) in [
        (top, b),
        (next, c),
        (foreground, a),
        (brought_child, 1),
        (child_hit, f.value(child_b)),
        (after_child, b),
        (raised, 1),
        (top_after_raise, a),
        (a_top, 8),
        (inherited, 8),
        (owned_top, d),
        (lowered, 1),
        (a_normal, 0),
        (d_normal, 0),
        (top_after_lower, b),
        (owned_promoted, 1),
        (owner_unchanged, 0),
        (d_top, 8),
        (owned_lowered, 1),
        (d_lowered, 0),
        (bottomed, 1),
        (b_normal, 0),
        (bottom_top, d),
        (brought, 1),
        (brought_top, c),
        (brought_foreground, c),
        (brought_active, c),
        (inactive, 0),
        (active_again, c),
        (invalid, 0),
        (error, 1400),
    ] {
        assert_eq!(f.value(slot), expected, "z-order result {slot:x}");
    }
    assert_eq!(f.value(no_send), 1);
    let no_send_messages: Vec<_> = (f.value(no_send_start)..f.value(no_send_end))
        .step_by(16)
        .filter(|&at| f.value(at) == c)
        .map(|at| f.value(at + 4))
        .collect();
    assert_eq!(no_send_messages, [wm::WINDOWPOSCHANGED]);
    assert_eq!(f.machine.ext.get::<Win32>().user.foreground_window(), c);
    assert_ne!(f.value(child_a), 0);
    for (start, end) in [(raise_start, raise_end), (lower_start, lower_end)] {
        let positions: Vec<_> = (f.value(start)..f.value(end))
            .step_by(16)
            .filter(|&at| f.value(at) == a)
            .map(|at| f.value(at + 4))
            .filter(|m| [wm::WINDOWPOSCHANGING, wm::WINDOWPOSCHANGED].contains(m))
            .collect();
        assert_eq!(positions, [wm::WINDOWPOSCHANGING, wm::WINDOWPOSCHANGED]);
    }
}
#[test]
// spec: WIN32-USER-7 WIN32-USER-19
fn topmost_bands_owned_popups_and_child_raise_have_standard_exit_paths() {
    ordering(JitCpu::new(PARK_ADDR, false).unwrap());
    ordering(JitCpu::new(PARK_ADDR, true).unwrap());
    ordering(UnicornCpu::new(PARK_ADDR).unwrap());
}
