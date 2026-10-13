//! Guest queue contracts only: no host window, keyboard or input injection.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Prot, Reg},
};
use adv_machine::{Clock, Machine};
use std::sync::Arc;

fn fixture() -> (Machine<JitCpu>, u32, Arc<adv_sync::input::KeyState>) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(".", "queue-keys.exe", "target/queue-key-contract-tests"),
    )
    .unwrap();
    for name in ["first", "second"] {
        machine
            .spawn(name, adv_machine::layout::PARK_ADDR, &[], 65536)
            .unwrap();
    }
    let memory = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, memory);
    machine.sys.current = 0;
    let live = Arc::new(adv_sync::input::KeyState::default());
    machine.ext.get_mut::<Win32>().user.host_keys = Some(live.clone());
    (machine, memory, live)
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
fn returned(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("expected an immediate queue query"),
    }
}
fn key(machine: &mut Machine<JitCpu>, vk: u32, asynchronous: bool) -> u32 {
    let stack = machine.cpu.reg(Reg::Esp);
    machine.cpu.write_u32(stack + 4, vk).unwrap();
    let mut env = env(machine);
    returned(if asynchronous {
        super::super::get_async_key_state(&mut env)
    } else {
        super::super::get_key_state(&mut env)
    })
}
fn keyboard(machine: &mut Machine<JitCpu>, output: u32) -> u32 {
    let stack = machine.cpu.reg(Reg::Esp);
    machine.cpu.write_u32(stack + 4, output).unwrap();
    returned(super::super::get_keyboard_state(&mut env(machine)))
}
fn next(machine: &mut Machine<JitCpu>, remove: bool) -> Msg {
    next_message(
        &mut env(machine),
        Filter {
            hwnd: 0,
            min: 0,
            max: 0,
        },
        remove,
    )
    .expect("authored posted input")
}

#[test]
fn message_extra_info_is_thread_local_and_only_successful_peeks_replace_it() {
    let (mut machine, _, _) = fixture();
    assert_eq!(get_message_extra_info(&mut env(&mut machine)), 0);
    assert_eq!(set_message_extra_info(&mut env(&mut machine), i32::MIN), 0);
    machine.sys.current = 1;
    assert_eq!(get_message_extra_info(&mut env(&mut machine)), 0);
    assert_eq!(
        set_message_extra_info(&mut env(&mut machine), 0x12345678),
        0
    );
    machine.sys.current = 0;
    assert_eq!(get_message_extra_info(&mut env(&mut machine)), i32::MIN);
    let filter = Filter {
        hwnd: 0,
        min: 0,
        max: 0,
    };
    assert!(next_message(&mut env(&mut machine), filter, false).is_none());
    assert_eq!(get_message_extra_info(&mut env(&mut machine)), i32::MIN);
    machine.ext.get_mut::<Win32>().user.queue(0).post(Msg {
        msg: 0x8000,
        ..Default::default()
    });
    assert!(
        next_message(
            &mut env(&mut machine),
            Filter {
                hwnd: 0,
                min: wm::KEYDOWN,
                max: wm::KEYUP
            },
            false
        )
        .is_none()
    );
    assert_eq!(get_message_extra_info(&mut env(&mut machine)), i32::MIN);
    assert!(next_message(&mut env(&mut machine), filter, false).is_some());
    assert_eq!(get_message_extra_info(&mut env(&mut machine)), 0);
    assert_eq!(set_message_extra_info(&mut env(&mut machine), -1), 0);
    assert!(next_message(&mut env(&mut machine), filter, true).is_some());
    assert_eq!(get_message_extra_info(&mut env(&mut machine)), 0);
    machine.sys.current = 1;
    assert_eq!(get_message_extra_info(&mut env(&mut machine)), 0x12345678);
    assert_eq!(
        get_message_extra_info_ABI.conv,
        adv_abi::Conv::Stdcall { argc: 0 }
    );
    assert_eq!(
        set_message_extra_info_ABI.conv,
        adv_abi::Conv::Stdcall { argc: 1 }
    );
}

#[test]
// spec: PERS-SCOPE-10
fn queued_down_remains_down_after_live_release_until_this_thread_removes_up() {
    let (mut machine, memory, live) = fixture();
    live.set(0x41, true);
    assert!(post_input(&mut env(&mut machine), 0, 0x100, 0x41, 1));
    live.set(0x41, false);
    assert!(post_input(
        &mut env(&mut machine),
        0,
        0x101,
        0x41,
        0xc000_0001,
    ));
    assert_eq!(key(&mut machine, 0x41, true), 0);
    assert_eq!(key(&mut machine, 0x41, false), 0);
    assert_eq!(next(&mut machine, false).msg, 0x100);
    assert_eq!(key(&mut machine, 0x41, false), 0, "PM_NOREMOVE");
    assert_eq!(next(&mut machine, true).msg, 0x100);
    assert_eq!(key(&mut machine, 0x41, false), 0xffff_8000);
    assert_eq!(
        key(&mut machine, 0x41, true),
        0,
        "physical input stays live"
    );
    assert_eq!(keyboard(&mut machine, memory + 256), 1);
    assert_eq!(machine.cpu.read_u8(memory + 256 + 0x41).unwrap(), 0x80);

    machine.sys.current = 1;
    assert_eq!(key(&mut machine, 0x41, false), 0);
    assert_eq!(keyboard(&mut machine, memory + 512), 1);
    assert_eq!(machine.cpu.read_u8(memory + 512 + 0x41).unwrap(), 0);
    // Application PostMessage cannot manufacture queue or physical key state.
    assert!(post(&mut env(&mut machine), 0, 0x100, 0x41, 1));
    assert_eq!(next(&mut machine, true).msg, 0x100);
    assert_eq!(key(&mut machine, 0x41, false), 0);
    assert_eq!(key(&mut machine, 0x41, true), 0);

    machine.sys.current = 0;
    assert_eq!(next(&mut machine, false).msg, 0x101);
    assert_eq!(key(&mut machine, 0x41, false), 0xffff_8000);
    assert_eq!(next(&mut machine, true).msg, 0x101);
    assert_eq!(key(&mut machine, 0x41, false), 0);
    assert_eq!(keyboard(&mut machine, memory + 256), 1);
    assert_eq!(machine.cpu.read_u8(memory + 256 + 0x41).unwrap(), 0);
}

#[test]
fn system_keys_and_repeats_toggle_caps_num_scroll_once_per_initial_down() {
    let (mut machine, _, _) = fixture();
    for vk in [0x14, 0x90, 0x91] {
        for (message, prior) in [(0x104, 1), (0x104, 0x4000_0001), (0x105, 0xc000_0001)] {
            assert!(post_input(&mut env(&mut machine), 0, message, vk, prior));
        }
        assert_eq!(next(&mut machine, false).msg, 0x104);
        assert_eq!(key(&mut machine, vk, false), 0);
        next(&mut machine, true);
        assert_eq!(key(&mut machine, vk, false), 0xffff_8001);
        next(&mut machine, true);
        assert_eq!(key(&mut machine, vk, false), 0xffff_8001, "repeat");
        next(&mut machine, true);
        assert_eq!(key(&mut machine, vk, false), 1, "system key up");
        for message in [0x100, 0x101] {
            assert!(post_input(&mut env(&mut machine), 0, message, vk, 1));
            next(&mut machine, true);
            assert_eq!(
                key(&mut machine, vk, false),
                if message == 0x100 { 0xffff_8000 } else { 0 }
            );
        }
    }
}

#[test]
fn swapped_mouse_queue_keys_are_logical_while_async_buttons_remain_physical() {
    let (mut machine, memory, live) = fixture();
    let window = machine.ext.get::<Win32>().user.desktop;
    let user = &mut machine.ext.get_mut::<Win32>().user;
    user.top.push(window);
    user.swap_buttons = true;
    live.set(1, true);
    assert!(super::super::host_input::post(
        &mut env(&mut machine),
        0x201,
        1,
        0
    ));
    assert_eq!(key(&mut machine, 1, true), 0x8000);
    assert_eq!(key(&mut machine, 2, false), 0);
    assert_eq!(next(&mut machine, false).msg, 0x204);
    assert_eq!(key(&mut machine, 2, false), 0);
    assert_eq!(next(&mut machine, true).msg, 0x204);
    assert_eq!(key(&mut machine, 2, false), 0xffff_8000);
    assert_eq!(key(&mut machine, 1, false), 0);
    assert_eq!(keyboard(&mut machine, memory + 256), 1);
    assert_eq!(machine.cpu.read_u8(memory + 257).unwrap(), 0);
    assert_eq!(machine.cpu.read_u8(memory + 258).unwrap(), 0x80);
    live.set(1, false);
    assert!(super::super::host_input::post(
        &mut env(&mut machine),
        0x202,
        0,
        0
    ));
    assert_eq!(key(&mut machine, 1, true), 0);
    assert_eq!(key(&mut machine, 2, false), 0xffff_8000);
    next(&mut machine, true);
    assert_eq!(key(&mut machine, 2, false), 0);
}

#[test]
fn translate_uses_consumed_shift_before_a_later_queued_release() {
    let (mut machine, _, live) = fixture();
    for (message, vk) in [(0x100, 0x10), (0x100, 0x41), (0x101, 0x10)] {
        assert!(post_input(&mut env(&mut machine), 0, message, vk, 1));
    }
    live.set(0x10, false);
    next(&mut machine, true);
    let character_key = next(&mut machine, true);
    assert_eq!(key(&mut machine, 0x10, false), 0xffff_8000);
    assert!(translate(&mut env(&mut machine), &character_key));
    let queue = &machine.ext.get::<Win32>().user.queues[&0];
    assert_eq!(queue.posted.back().unwrap().msg, wm::CHAR);
    assert_eq!(queue.posted.back().unwrap().wparam, u32::from(b'A'));
    assert!(!queue.posted.back().unwrap().input);
    next(&mut machine, true);
    assert_eq!(key(&mut machine, 0x10, false), 0);
}

#[test]
fn keyboard_output_fault_checks_the_full_span_and_preserves_its_writable_prefix() {
    let (mut machine, memory, _) = fixture();
    let output = memory + 4096 - 128;
    machine.cpu.write(output, &[0xa5; 256]).unwrap();
    machine
        .cpu
        .protect(memory + 4096, 4096, Prot::READ)
        .unwrap();
    assert_eq!(keyboard(&mut machine, output), 0);
    let mut bytes = [0; 256];
    machine.cpu.read(output, &mut bytes).unwrap();
    assert_eq!(bytes, [0xa5; 256]);
    assert_eq!(keyboard(&mut machine, 0), 0);
}

#[test]
// spec: PERS-SCOPE-10
fn set_keyboard_state_changes_only_this_queue_and_preserves_pending_input() {
    let (mut machine, memory, live) = fixture();
    let input = memory + 256;
    let output = memory + 512;
    let bytes = std::array::from_fn::<_, 256, _>(|i| i as u8);
    machine.cpu.write(input, &bytes).unwrap();
    live.set(0x41, true);
    assert!(post_input(&mut env(&mut machine), 0, wm::KEYUP, 0x80, 1));
    crate::util::set_last_error(&mut env(&mut machine), 1234);
    assert_eq!(
        super::super::input::set_keyboard_state(
            &mut env(&mut machine),
            adv_abi::GuestPtr::new(input)
        ),
        1
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 1234);
    assert_eq!(keyboard(&mut machine, output), 1);
    let mut actual = [0; 256];
    machine.cpu.read(output, &mut actual).unwrap();
    assert_eq!(actual, bytes.map(|value| value & 0x81));
    assert_eq!(key(&mut machine, 0x41, true), 0x8000);
    assert_eq!(key(&mut machine, 0x41, false), 1);
    machine.sys.current = 1;
    assert_eq!(key(&mut machine, 0x81, false), 0);
    machine.sys.current = 0;
    assert_eq!(key(&mut machine, 0x80, false), 0xffff_8000);
    assert_eq!(next(&mut machine, false).msg, wm::KEYUP);
    assert_eq!(key(&mut machine, 0x80, false), 0xffff_8000);
    next(&mut machine, true);
    assert_eq!(key(&mut machine, 0x80, false), 0);
}

#[test]
fn set_keyboard_state_bad_input_leaves_the_existing_queue_state_intact() {
    let (mut machine, memory, _) = fixture();
    let input = memory + 4096 - 128;
    machine.cpu.write(input, &[0x81; 256]).unwrap();
    machine.ext.get_mut::<Win32>().user.queue(0).keyboard.0[0x41] = 0x80;
    machine.cpu.protect(memory + 4096, 4096, Prot(0)).unwrap();
    for pointer in [0, input] {
        assert_eq!(
            super::super::input::set_keyboard_state(
                &mut env(&mut machine),
                adv_abi::GuestPtr::new(pointer)
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 998);
        assert_eq!(key(&mut machine, 0x41, false), 0xffff_8000);
        assert_eq!(key(&mut machine, 0x80, false), 0);
    }
}
