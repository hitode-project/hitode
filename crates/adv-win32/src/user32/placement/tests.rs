use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt},
};
use adv_machine::{Clock, Machine};

fn fixture() -> (Machine<JitCpu>, u32, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(".", "placement-fixture.exe", "target/placement-tests"),
    )
    .unwrap();
    machine
        .spawn("placement", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let memory = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    let hwnd = machine.ext.get::<crate::Win32>().user.desktop;
    let window = machine
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut machine.sys.objects, &hwnd)
        .unwrap();
    window.style = WS_CAPTION | WS_DLGFRAME;
    window.rect = Rect::new(150, 170, 360, 260);
    window.placement = PlacementState::new(window.rect);
    (machine, memory, hwnd)
}

fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

fn snapshot(machine: &mut Machine<JitCpu>, memory: u32, hwnd: u32) -> [i32; 11] {
    assert_eq!(
        get_window_placement(&mut env(machine), Handle(hwnd), GuestPtr::new(memory)),
        1
    );
    std::array::from_fn(|i| machine.cpu.read_u32(memory + i as u32 * 4).unwrap() as i32)
}

#[test]
fn output_is_a_complete_44_byte_write_preserving_error_and_guard_bytes() {
    let (mut machine, memory, hwnd) = fixture();
    for length in [0, 1, 43, 44, 45, 60] {
        machine.cpu.write(memory, &[0xa5; 48]).unwrap();
        machine.cpu.write_u32(memory, length).unwrap();
        set_last_error(&mut env(&mut machine), 0x1234);
        assert_eq!(
            snapshot(&mut machine, memory, hwnd),
            [44, 0, 1, -1, -1, -1, -1, 150, 170, 510, 430]
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 0x1234);
        assert_eq!(machine.cpu.read_u32(memory + 44).unwrap(), 0xa5a5a5a5);
    }
}

#[test]
fn show_move_minimize_and_restore_keep_one_normal_rectangle_and_maximize_history() {
    let (mut machine, memory, hwnd) = fixture();
    let window = machine
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut machine.sys.objects, &hwnd)
        .unwrap();
    window.rect = Rect::new(250, 270, 460, 360);
    position_changed(window);
    assert_eq!(
        show_state(window, SW_MAXIMIZE, (1920, 1080), (1920, 1040)),
        Some(2)
    );
    assert_eq!(
        snapshot(&mut machine, memory, hwnd),
        [44, 2, 3, -1, -1, -1, -1, 250, 270, 710, 630]
    );
    let window = machine
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut machine.sys.objects, &hwnd)
        .unwrap();
    assert_eq!(
        show_state(window, SW_MINIMIZE, (1920, 1080), (1920, 1040)),
        Some(1)
    );
    assert_eq!(
        snapshot(&mut machine, memory, hwnd),
        [44, 2, 2, -32000, -32000, -1, -1, 250, 270, 710, 630]
    );
    let window = machine
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut machine.sys.objects, &hwnd)
        .unwrap();
    show_state(window, SW_RESTORE, (1920, 1080), (1920, 1040));
    assert_eq!(window.rect, Rect::new(0, 0, 1920, 1040));
    assert_eq!(window.style & (WS_MINIMIZE | WS_MAXIMIZE), WS_MAXIMIZE);
    show_state(window, SW_SHOWNORMAL, (1920, 1080), (1920, 1040));
    assert_eq!(window.rect, Rect::new(250, 270, 460, 360));
    assert_eq!(snapshot(&mut machine, memory, hwnd)[..3], [44, 2, 1]);
    let window = machine
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut machine.sys.objects, &hwnd)
        .unwrap();
    show_state(window, SW_SHOWMINNOACTIVE, (1920, 1080), (1920, 1040));
    show_state(window, SW_SHOWNOACTIVATE, (1920, 1080), (1920, 1040));
    assert_eq!(
        snapshot(&mut machine, memory, hwnd),
        [44, 0, 1, -32000, -32000, -1, -1, 250, 270, 710, 630]
    );
}

#[test]
fn invalid_handle_and_unwritable_ranges_fail_without_partial_output() {
    let (mut machine, memory, hwnd) = fixture();
    machine
        .cpu
        .protect(memory + 4096, 4096, Prot::READ)
        .unwrap();
    for (handle, output, error) in [
        (0, 0, 1400),
        (0x1234, memory, 1400),
        (hwnd, 0, 998),
        (hwnd, u32::MAX - 20, 998),
        (hwnd, memory + 4096 - 24, 998),
    ] {
        machine.cpu.write(memory, &[0xa5; 64]).unwrap();
        assert_eq!(
            get_window_placement(
                &mut env(&mut machine),
                Handle(handle),
                GuestPtr::new(output)
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), error);
        let mut guard = [0; 64];
        machine.cpu.read(memory, &mut guard).unwrap();
        assert_eq!(guard, [0xa5; 64]);
    }
    machine.cpu.write(memory, &[0xa5; 64]).unwrap();
    machine.cpu.protect(memory, 4096, Prot::READ).unwrap();
    assert_eq!(
        get_window_placement(&mut env(&mut machine), Handle(hwnd), GuestPtr::new(memory)),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 998);
    let mut guard = [0; 64];
    machine.cpu.read(memory, &mut guard).unwrap();
    assert_eq!(guard, [0xa5; 64]);
}

#[test]
fn tool_and_child_minimum_positions_use_the_same_virtual_geometry_metrics() {
    let (mut machine, memory, hwnd) = fixture();
    let window = machine
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut machine.sys.objects, &hwnd)
        .unwrap();
    window.ex_style = 0x80;
    show_state(window, SW_SHOWMINNOACTIVE, (1920, 1080), (1920, 1040));
    assert_eq!(&snapshot(&mut machine, memory, hwnd)[3..5], &[0, 1011]);
    let window = machine
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut machine.sys.objects, &hwnd)
        .unwrap();
    window.rect = Rect::new(350, 370, 460, 360);
    position_changed(window);
    show_state(window, SW_RESTORE, (1920, 1080), (1920, 1040));
    assert_eq!(window.rect, Rect::new(150, 170, 360, 260));
    assert_eq!(&snapshot(&mut machine, memory, hwnd)[3..5], &[350, 370]);
    let window = machine
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut machine.sys.objects, &hwnd)
        .unwrap();
    window.parent = 1;
    window.ex_style = 0;
    window.style &= !WS_MINIMIZE;
    window.placement = PlacementState::new(window.rect);
    show_state(window, SW_SHOWMINNOACTIVE, (1920, 1080), (500, 400));
    assert_eq!(&snapshot(&mut machine, memory, hwnd)[3..5], &[0, 371]);
}
