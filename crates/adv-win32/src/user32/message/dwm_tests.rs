//! Authored lifecycle continuations and queue observations, without host USER/GPU.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Prot, Reg},
};
use adv_machine::{Clock, Machine, Resume};

fn fixture() -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/dwm-queue-tests");
    Win32::install(
        &mut m,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    m.spawn("authored", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let stack = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, stack);
    m.ext
        .get_mut::<Win32>()
        .user
        .register_class("AuthoredDwm", 0, 1, 0, 0, 0, 0, 0);
    m
}
fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn begin(m: &mut Machine<JitCpu>, style: u32, parent: u32) -> Outcome {
    window::create_window(
        &mut env(m),
        window::CreateParams {
            ex_style: 0,
            class: window::ClassKey::Name("AuthoredDwm".into()),
            class_ptr: 0,
            name_ptr: 0,
            text: String::new(),
            style,
            x: 0,
            y: 0,
            w: 100,
            h: 80,
            parent,
            menu: 0,
            instance: 0,
            param: 0,
        },
        |_, hwnd| Outcome::Return(hwnd),
    )
}
fn complete(m: &mut Machine<JitCpu>, mut out: Outcome, fail: Option<u32>) -> (u32, Vec<u32>) {
    let mut messages = Vec::new();
    for _ in 0..32 {
        out = match out {
            Outcome::Return(hwnd) => return (hwnd, messages),
            Outcome::Call { args, then, .. } => {
                messages.push(args[1]);
                let result = if fail == Some(args[1]) {
                    if args[1] == wm::NCCREATE { 0 } else { u32::MAX }
                } else {
                    u32::from(args[1] == wm::NCCREATE)
                };
                then(&mut env(m), Resume::Returned(result))
            }
            _ => panic!("unexpected lifecycle outcome"),
        };
    }
    panic!("unbounded lifecycle callbacks");
}
fn create(m: &mut Machine<JitCpu>, style: u32, parent: u32) -> u32 {
    let out = begin(m, style, parent);
    complete(m, out, None).0
}
fn next(m: &mut Machine<JitCpu>, remove: bool) -> Option<Msg> {
    next_message(
        &mut env(m),
        Filter {
            hwnd: 0,
            min: 0,
            max: 0,
        },
        remove,
    )
}
fn status(m: &mut Machine<JitCpu>) -> u32 {
    let sp = m.cpu.reg(Reg::Esp);
    m.cpu.write_u32(sp + 4, 0x5ff).unwrap();
    match get_queue_status(&mut env(m)) {
        Outcome::Return(value) => value,
        _ => panic!("queue status blocked"),
    }
}
fn signaled(m: &Machine<JitCpu>, handle: u32) -> bool {
    matches!(
        m.sys.objects.get(handle),
        Some(Object::Event { signaled: true, .. })
    )
}

#[test]
fn initial_dwm_post_follows_nccreate_and_precedes_create_without_physical_input() {
    let mut m = fixture();
    let out = begin(&mut m, 0, 0);
    let Outcome::Call { args, then, .. } = out else {
        panic!("NCCREATE missing")
    };
    assert_eq!(args[1], wm::NCCREATE);
    assert_eq!(queue_status(&mut env(&mut m)), (0, 0));
    let out = then(&mut env(&mut m), Resume::Returned(1));
    let Outcome::Call { args, .. } = &out else {
        panic!("CREATE missing")
    };
    assert_eq!(args[1], wm::CREATE);
    assert_eq!(queue_status(&mut env(&mut m)), (8, 8));
    let (hwnd, _) = complete(&mut m, out, None);
    let message = next(&mut m, true).unwrap();
    assert_eq!(
        (message.hwnd, message.msg, message.wparam, message.lparam),
        (hwnd, wm::DWMNCRENDERINGCHANGED, 1, 0)
    );
    assert!(!message.input);
    assert_eq!(message.input_receipt, 0);
    assert!(message.input_key.is_none());
    assert!(
        m.ext.get::<Win32>().user.queues[&0]
            .keyboard
            .0
            .iter()
            .all(|&k| k == 0)
    );
    assert!(next(&mut m, true).is_none());
    // The same number posted by the application retains application categories.
    assert!(post(
        &mut env(&mut m),
        hwnd,
        wm::DWMNCRENDERINGCHANGED,
        1,
        0
    ));
    assert_eq!(status(&mut m), 0x0108_0108);
}

#[test]
fn decorated_top_levels_only_receive_initial_composition_notification() {
    use window::{WS_BORDER, WS_CAPTION, WS_CHILD, WS_POPUP, WS_THICKFRAME};
    let mut m = fixture();
    let parent = create(&mut m, 0, 0);
    next(&mut m, true).unwrap();
    for (style, owner, expected) in [
        (0, 0, true),
        (0x02cb_0000, 0, true),
        (WS_POPUP, 0, false),
        (WS_POPUP | WS_CAPTION, 0, true),
        (WS_POPUP | WS_BORDER, 0, false),
        (WS_POPUP | WS_THICKFRAME, 0, true),
        (WS_CHILD | WS_CAPTION, parent, false),
        (0, (-3i32) as u32, false),
    ] {
        let hwnd = create(&mut m, style, owner);
        assert_ne!(hwnd, 0);
        assert_eq!(
            next(&mut m, true).map(|msg| (msg.hwnd, msg.msg)),
            expected.then_some((hwnd, wm::DWMNCRENDERINGCHANGED)),
            "style {style:x}, parent {owner:x}"
        );
    }
}

#[test]
fn rejected_creation_and_destroyed_windows_cannot_dispatch_stale_system_posts() {
    let mut m = fixture();
    let baseline = m.ext.get::<Win32>().user.windows.len();
    for (failure, expected_messages, expected_status) in [
        (wm::NCCREATE, vec![wm::NCCREATE, wm::NCDESTROY], 0),
        (
            wm::CREATE,
            vec![wm::NCCREATE, wm::CREATE, wm::DESTROY, wm::NCDESTROY],
            0x0008_0008,
        ),
    ] {
        let out = begin(&mut m, 0, 0);
        let (result, messages) = complete(&mut m, out, Some(failure));
        assert_eq!(result, 0);
        assert_eq!(messages, expected_messages);
        assert_eq!(m.ext.get::<Win32>().user.windows.len(), baseline);
        assert_eq!(status(&mut m), expected_status);
        assert!(next(&mut m, false).is_none());
        assert_eq!(status(&mut m), 0);
    }
    let old = create(&mut m, 0, 0);
    let out = destroy_window(&mut env(&mut m), old, |_, result| {
        Outcome::Return(u32::from(result))
    });
    complete(&mut m, out, None);
    assert_eq!(m.ext.get::<Win32>().user.windows.len(), baseline);
    assert_eq!(status(&mut m), 0x0008_0008);
    assert!(next(&mut m, true).is_none());
    let new = create(&mut m, 0, 0);
    assert_ne!(new, old);
    assert_eq!(next(&mut m, true).unwrap().hwnd, new);
    assert!(next(&mut m, true).is_none());
}

#[test]
fn initial_post_is_new_until_peek_or_queue_status_observes_it() {
    let mut m = fixture();
    let hwnd = create(&mut m, 0, 0);
    let (input, _) = message_wait_events(&mut env(&mut m), 0x4bf).unwrap();
    assert!(signaled(&m, input));
    message_wait_events(&mut env(&mut m), 0x4bf).unwrap();
    assert!(signaled(&m, input), "waiting alone does not mark input old");
    assert_eq!(next(&mut m, false).unwrap().hwnd, hwnd);
    message_wait_events(&mut env(&mut m), 0x4bf).unwrap();
    assert!(
        !signaled(&m, input),
        "PM_NOREMOVE observes newness without dequeuing"
    );
    assert_eq!(next(&mut m, true).unwrap().hwnd, hwnd);
    let other = create(&mut m, 0, 0);
    assert_eq!(status(&mut m), 0x0008_0008);
    message_wait_events(&mut env(&mut m), 0x4bf).unwrap();
    assert!(!signaled(&m, input), "GetQueueStatus also marks input old");
    assert_eq!(next(&mut m, true).unwrap().hwnd, other);
}
