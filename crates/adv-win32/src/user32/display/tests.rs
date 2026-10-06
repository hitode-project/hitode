use super::*;
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg},
};
use adv_machine::{Clock, Machine};
use window::{ControlState, WS_CAPTION, WS_THICKFRAME, WS_VISIBLE, Window};

fn fixture() -> Machine<JitCpu> {
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(".", "fullscreen-test.exe", "target/fullscreen-test"),
    )
    .unwrap();
    let procedure = machine
        .resolve_import("user32.dll", "DefWindowProcA")
        .unwrap();
    let stack = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, stack);
    let window = Window {
        hwnd: 0x10020,
        class: 0,
        proc: procedure,
        unicode: false,
        style: WS_CAPTION | WS_THICKFRAME | WS_VISIBLE,
        ex_style: window::WS_EX_CLIENTEDGE,
        parent: 0,
        owner: 0,
        id: 0,
        instance: 0,
        rect: Rect::new(560, 240, 820, 643),
        placement: super::super::placement::PlacementState::new(Rect::new(560, 240, 820, 643)),
        text: String::new(),
        extra: Vec::new(),
        user_data: 0,
        thread: 0,
        children: Vec::new(),
        font: 0,
        needs_paint: false,
        created_us: 0,
        initial_geometry_reported: true,
        destroying: false,
        control: ControlState::default(),
        auto_clicks: 0,
        hidden_popup: false,
    };
    let user = &mut machine.ext.get_mut::<Win32>().user;
    assert!(
        user.windows
            .insert(&mut machine.sys.objects, window.hwnd, window)
    );
    user.top.push(0x10020);
    machine
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

fn finish(machine: &mut Machine<JitCpu>, outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        Outcome::Call {
            target, args, then, ..
        } => {
            let thunk =
                (target - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
            let handler = machine.sys.thunks.get(thunk).f.handler;
            let stack = machine.cpu.reg(Reg::Esp);
            for (i, arg) in args.into_iter().enumerate() {
                machine
                    .cpu
                    .write_u32(stack + 4 + i as u32 * 4, arg)
                    .unwrap();
            }
            let mut context = env(machine);
            context.thunk = thunk;
            let result = handler(&mut context);
            let value = finish(machine, result);
            let result = then(&mut env(machine), adv_machine::Resume::Returned(value));
            finish(machine, result)
        }
        Outcome::Fail(error) => panic!("window callback failed: {error}"),
        _ => panic!("unexpected window callback outcome"),
    }
}

fn enter(machine: &mut Machine<JitCpu>, width: u32, height: u32) {
    let outcome = fullscreen(&mut env(machine), 1, 0x10020, width, height);
    assert_eq!(finish(machine, outcome), 0);
}
fn assert_geometry(
    machine: &Machine<JitCpu>,
    display: (i32, i32),
    origin: (i32, i32),
    client: (i32, i32),
) {
    let user = &machine.ext.get::<Win32>().user;
    assert_eq!(user.screen, display);
    let desktop = user
        .windows
        .get(&machine.sys.objects, &user.desktop)
        .unwrap();
    assert_eq!((desktop.rect.width(), desktop.rect.height()), display);
    assert_eq!(user.client_origin(&machine.sys.objects, 0x10020), origin);
    assert_eq!(
        user.windows
            .get(&machine.sys.objects, &0x10020)
            .unwrap()
            .client_size(),
        client
    );
}

#[test]
fn fullscreen_mouse_uses_display_origin_and_windowed_reset_restores_borders() {
    let mut machine = fixture();
    let original = machine
        .ext
        .get::<Win32>()
        .user
        .windows
        .get(&machine.sys.objects, &0x10020)
        .unwrap();
    let (rect, style, ex_style) = (original.rect, original.style, original.ex_style);
    enter(&mut machine, 800, 600);
    assert_geometry(&machine, (800, 600), (0, 0), (800, 600));
    assert!(super::super::host_input::post(
        &mut env(&mut machine),
        0x200,
        0,
        window::make_long(650, 410)
    ));
    assert_eq!(machine.ext.get::<Win32>().user.cursor_pos, (650, 410));
    let outcome = windowed(&mut env(&mut machine), 1);
    assert_eq!(finish(&mut machine, outcome), 0);
    let window = machine
        .ext
        .get::<Win32>()
        .user
        .windows
        .get(&machine.sys.objects, &0x10020)
        .unwrap();
    assert_eq!(
        (
            window.rect.left,
            window.rect.top,
            window.rect.width(),
            window.rect.height()
        ),
        (rect.left, rect.top, rect.width(), rect.height())
    );
    assert_eq!((window.style, window.ex_style), (style, ex_style));
    assert_geometry(&machine, (1920, 1080), (570, 273), (800, 600));
}

#[test]
fn fullscreen_resizes_do_not_replace_saved_window_and_final_release_restores_it() {
    let mut machine = fixture();
    for (width, height) in [(800, 600), (1024, 768), (640, 480)] {
        enter(&mut machine, width, height);
        assert_geometry(
            &machine,
            (width as i32, height as i32),
            (0, 0),
            (width as i32, height as i32),
        );
    }
    release(&mut env(&mut machine), 1);
    assert_geometry(&machine, (1920, 1080), (570, 273), (800, 600));
    assert!(machine.ext.get::<Modes>().owners.is_empty());
    release(&mut env(&mut machine), 1);
    assert_geometry(&machine, (1920, 1080), (570, 273), (800, 600));
}

#[test]
fn fullscreen_preserves_maximized_windows_normal_restore_rectangle() {
    let mut machine = fixture();
    let outcome = window::show_window(
        &mut env(&mut machine),
        0x10020,
        window::SW_MAXIMIZE,
        |_, _| Outcome::Return(0),
    );
    assert_eq!(finish(&mut machine, outcome), 0);
    enter(&mut machine, 800, 600);
    let outcome = windowed(&mut env(&mut machine), 1);
    assert_eq!(finish(&mut machine, outcome), 0);
    let outcome = window::show_window(
        &mut env(&mut machine),
        0x10020,
        window::SW_RESTORE,
        |_, _| Outcome::Return(0),
    );
    assert_eq!(finish(&mut machine, outcome), 0);
    assert_geometry(&machine, (1920, 1080), (570, 273), (800, 600));
}

#[test]
fn shared_fullscreen_window_restores_surviving_device_before_windowed_baseline() {
    let mut machine = fixture();
    enter(&mut machine, 800, 600);
    let outcome = fullscreen(&mut env(&mut machine), 2, 0x10020, 1024, 768);
    assert_eq!(finish(&mut machine, outcome), 0);
    assert_geometry(&machine, (1024, 768), (0, 0), (1024, 768));
    let outcome = windowed(&mut env(&mut machine), 2);
    assert_eq!(finish(&mut machine, outcome), 0);
    assert_geometry(&machine, (800, 600), (0, 0), (800, 600));
    release(&mut env(&mut machine), 1);
    assert_geometry(&machine, (1920, 1080), (570, 273), (800, 600));
}

#[test]
fn display_queries_and_mouse_polling_observe_the_committed_fullscreen_mode() {
    let mut machine = fixture();
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    enter(&mut machine, 800, 600);
    assert!(super::super::host_input::post(
        &mut env(&mut machine),
        0x201,
        1,
        window::make_long(799, 599)
    ));
    assert_eq!(machine.ext.get::<Win32>().user.cursor_pos, (799, 599));
    let desktop = machine.ext.get::<Win32>().user.desktop;
    assert!(
        machine
            .ext
            .get::<Win32>()
            .user
            .screen_rect(&machine.sys.objects, desktop)
            .unwrap()
            .write(&mut machine.cpu, data)
    );
    assert_eq!(machine.cpu.read_u32(data + 8).unwrap(), 800);
    assert_eq!(machine.cpu.read_u32(data + 12).unwrap(), 600);
}
