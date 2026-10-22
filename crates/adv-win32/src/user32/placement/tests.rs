use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt},
};
use adv_machine::{Clock, Machine};

fn finish(machine: &mut Machine<JitCpu>, mut outcome: Outcome, messages: &mut Vec<u32>) -> u32 {
    for _ in 0..64 {
        match outcome {
            Outcome::Return(value) => return value,
            Outcome::Call { args, then, .. } => {
                messages.push(args[1]);
                outcome = then(&mut env(machine), adv_machine::Resume::Returned(0));
            }
            _ => panic!("same-thread placement must not suspend"),
        }
    }
    panic!("unbounded placement callbacks");
}
fn input(machine: &mut Machine<JitCpu>, memory: u32, fields: [i32; 11]) {
    for (i, field) in fields.iter().enumerate() {
        machine
            .cpu
            .write_u32(memory + i as u32 * 4, *field as u32)
            .unwrap();
    }
}

#[test]
// spec: WIN32-USER-6 WIN32-USER-7
fn setter_reads_only_a_complete_structure_and_routes_geometry_through_user_callbacks() {
    let (mut m, memory, hwnd) = fixture();
    let fields = [44, 8, 4, 10, 20, 30, 40, 250, 270, 710, 630];
    input(&mut m, memory, fields);
    m.cpu.write_u32(memory + 44, 0xcccccccc).unwrap();
    m.cpu.protect(memory, 4096, Prot::READ).unwrap();
    let mut messages = Vec::new();
    let outcome = set_window_placement(&mut env(&mut m), Handle(hwnd), GuestPtr::new(memory));
    assert_eq!(finish(&mut m, outcome, &mut messages), 1);
    assert!(messages.contains(&0x46) && messages.contains(&0x47));
    assert_eq!(
        snapshot(&mut m, memory + 4096, hwnd),
        [44, 0, 1, 10, 20, -1, -1, 250, 270, 710, 630]
    );
    assert_eq!(m.cpu.read_u32(memory + 44).unwrap(), 0xcccccccc);
    for (i, value) in fields.iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(memory + i as u32 * 4).unwrap(),
            *value as u32
        );
    }
}

#[test]
// spec: WIN32-USER-7
fn setter_pairs_minimize_maximize_restore_and_explicit_minimum_positions() {
    let (mut m, memory, hwnd) = fixture();
    let mut messages = Vec::new();
    for (flags, show, expected) in [(0, 3, [2, 3]), (2, 2, [2, 2]), (0, 9, [0, 1])] {
        input(
            &mut m,
            memory,
            [44, flags, show, 10, 20, -1, -1, 250, 270, 710, 630],
        );
        let outcome = set_window_placement(&mut env(&mut m), Handle(hwnd), GuestPtr::new(memory));
        assert_eq!(finish(&mut m, outcome, &mut messages), 1);
        let actual = snapshot(&mut m, memory + 4096, hwnd);
        assert_eq!(actual[1..3], expected);
        assert_eq!(actual[7..], [250, 270, 710, 630]);
    }
    m.ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &hwnd)
        .unwrap()
        .ex_style = 0x80;
    input(
        &mut m,
        memory,
        [44, 1, 7, 10, 20, -1, -1, 250, 270, 710, 630],
    );
    let outcome = set_window_placement(&mut env(&mut m), Handle(hwnd), GuestPtr::new(memory));
    assert_eq!(finish(&mut m, outcome, &mut messages), 1);
    assert_eq!(snapshot(&mut m, memory + 4096, hwnd)[1..5], [0, 2, 10, 20]);
}

#[test]
// spec: WIN32-USER-6 WIN32-USER-7
fn setter_recovers_offscreen_normal_windows_but_preserves_tool_coordinates() {
    for (tool, expected) in [
        (false, [1460, 680, 1920, 1040]),
        (true, [30000, 30000, 30460, 30360]),
    ] {
        let (mut m, memory, hwnd) = fixture();
        m.ext
            .get::<crate::Win32>()
            .user
            .windows
            .get_mut(&mut m.sys.objects, &hwnd)
            .unwrap()
            .ex_style = if tool { 0x80 } else { 0 };
        input(
            &mut m,
            memory,
            [44, 0, 4, -1, -1, -1, -1, 30000, 30000, 30460, 30360],
        );
        let outcome = set_window_placement(&mut env(&mut m), Handle(hwnd), GuestPtr::new(memory));
        assert_eq!(finish(&mut m, outcome, &mut Vec::new()), 1);
        assert_eq!(snapshot(&mut m, memory + 4096, hwnd)[7..], expected);
    }
}

#[test]
// spec: WIN32-USER-6 WIN32-USER-7
fn setter_rejects_bad_lengths_and_unreadable_input_without_changing_window() {
    let (mut m, memory, hwnd) = fixture();
    let original = snapshot(&mut m, memory + 4096, hwnd);
    for length in [0, 1, 43, 45, 60, -1] {
        input(
            &mut m,
            memory,
            [length, 0, 4, 10, 20, -1, -1, 250, 270, 710, 630],
        );
        let outcome = set_window_placement(&mut env(&mut m), Handle(hwnd), GuestPtr::new(memory));
        assert_eq!(finish(&mut m, outcome, &mut Vec::new()), 0);
        assert_eq!(crate::util::last_error(&env(&mut m)), 87);
        assert_eq!(snapshot(&mut m, memory + 4096, hwnd), original);
    }
    for (target, pointer, error) in [(0, 0, 1400), (hwnd, 0, 998), (hwnd, u32::MAX - 20, 998)] {
        let outcome =
            set_window_placement(&mut env(&mut m), Handle(target), GuestPtr::new(pointer));
        assert_eq!(finish(&mut m, outcome, &mut Vec::new()), 0);
        assert_eq!(crate::util::last_error(&env(&mut m)), error);
        assert_eq!(snapshot(&mut m, memory + 4096, hwnd), original);
    }
}

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
