use super::*;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{
    Win32, Win32Config,
    user32::window::{Rect, WS_POPUP, WS_VISIBLE, Window},
};

const HWND: u32 = 0x10020;

fn provider(ext: &Ext, objects: &Objects, hwnd: u32) -> Option<InputSnapshot> {
    let user = &ext.get::<Win32>().user;
    user.windows.get(objects, &hwnd)?;
    Some(InputSnapshot {
        keys: user
            .host_keys
            .as_ref()
            .map_or(user.keys, |keys| keys.snapshot()),
        cursor: user.cursor_pos,
        foreground: user.top.first() == Some(&hwnd),
    })
}
fn fixture(jit: bool) -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    super::super::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            ".",
            "legacy-input.exe",
            "target/investigations/idea/input-fixture",
        ),
    )
    .unwrap();
    let rect = Rect::new(100, 50, 640, 480);
    let mut window = Window::new(HWND, 0, 0, WS_POPUP | WS_VISIBLE, 0, 0, rect, 0);
    window.instance = 0x400000;
    let user = &mut m.ext.get_mut::<Win32>().user;
    assert!(user.windows.insert(&mut m.sys.objects, HWND, window));
    user.top.push(HWND);
    m.ext.insert(InputProvider(provider));
    let data = m.sys.vm.alloc(&mut m.cpu, 8192, Prot::RW).unwrap();
    (m, data)
}
fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn make_device(m: &mut Machine<JitCpu>, data: u32, kind: Iid, wide: bool) -> (u32, u32) {
    assert_eq!(
        create_legacy(
            &mut env(m),
            Handle(0x400000),
            0x500,
            GuestPtr::new(data),
            GuestPtr::new(0),
            wide
        ),
        0
    );
    let factory = m.cpu.read_u32(data).unwrap();
    m.cpu.write(data + 16, &kind).unwrap();
    assert_eq!(
        create_device(
            &mut env(m),
            factory,
            GuestPtr::new(data + 16),
            GuestPtr::new(data + 4),
            GuestPtr::new(0),
            wide
        ),
        0
    );
    (factory, m.cpu.read_u32(data + 4).unwrap())
}
fn format(
    m: &mut Machine<JitCpu>,
    data: u32,
    size: u32,
    objects: &[(Option<Iid>, u32, u32)],
) -> u32 {
    let pointer = data + 256;
    let entries = data + 512;
    for (index, &(guid, offset, kind)) in objects.iter().enumerate() {
        let guid_pointer = data + 2048 + index as u32 * 16;
        if let Some(guid) = guid {
            m.cpu.write(guid_pointer, &guid).unwrap();
        }
        for (word, value) in [
            if guid.is_some() { guid_pointer } else { 0 },
            offset,
            kind,
            0,
        ]
        .into_iter()
        .enumerate()
        {
            m.cpu
                .write_u32(entries + index as u32 * 16 + word as u32 * 4, value)
                .unwrap();
        }
    }
    for (index, word) in [24, 16, 2, size, objects.len() as u32, entries]
        .into_iter()
        .enumerate()
    {
        m.cpu.write_u32(pointer + index as u32 * 4, word).unwrap();
    }
    pointer
}
fn keyboard_format(m: &mut Machine<JitCpu>, data: u32) -> u32 {
    format(
        m,
        data,
        256,
        &[(Some(KEY), 30, 0x1e0c), (Some(KEY), 42, 0x2a0c)],
    )
}
fn mouse_format(m: &mut Machine<JitCpu>, data: u32) -> u32 {
    let mut objects = Vec::new();
    for axis in 0..3 {
        let mut guid = [
            0xe0, 0x02, 0x6d, 0xa3, 0xf3, 0xc9, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53, 0x54, 0,
            0,
        ];
        guid[0] += axis;
        objects.push((Some(guid), u32::from(axis) * 4, 0xffff03));
    }
    for button in 0..4 {
        objects.push((Some(BUTTON), 12 + button, 0xffff0c));
    }
    format(m, data, 16, &objects)
}
fn configure(m: &mut Machine<JitCpu>, device: u32, format: u32) {
    assert_eq!(set_format(&mut env(m), device, format), 0);
    assert_eq!(set_cooperative(&mut env(m), device, HWND, 6), 0);
}
fn method(code: &mut Asm, object_pointer: u32, slot: u8, args: &[u32], result: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.mov_eax_mem(object_pointer)
        .push_eax()
        .mov_eax_ptr_eax();
    code.bytes.extend([0xff, 0x50, slot * 4]); // call [eax + slot*4]
    code.mov_mem_eax(result);
}

#[test]
fn legacy_controller_enumeration_is_empty_without_callback_or_owner_side_effects() {
    for jit in [false, true] {
        for wide in [false, true] {
            let (mut m, data) = fixture(jit);
            m.ext.get_or_default::<adv_abi::ApiDiagnostics>();
            let before = m.sys.objects.object_count();
            let code_address = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RX).unwrap();
            let callback = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RX).unwrap();
            let mut counter = Asm::new(callback);
            counter.bytes.extend([0xff, 0x05]); // inc dword [callback count]
            counter.bytes.extend((data + 64).to_le_bytes());
            counter.bytes.extend([0x31, 0xc0]);
            counter.ret_n(8);
            m.cpu.write(callback, &counter.bytes).unwrap();
            m.cpu.write_u32(data + 68, 0x12345678).unwrap();
            let create = m
                .resolve_import(
                    "dinput.dll",
                    if wide {
                        "DirectInputCreateW"
                    } else {
                        "DirectInputCreateA"
                    },
                )
                .unwrap();
            let mut code = Asm::new(code_address);
            code.bytes.extend([0x89, 0xe0]);
            code.mov_mem_eax(data + 72);
            for arg in [0, data, 0x500, 0x400000] {
                code.push_imm(arg);
            }
            code.call(create).mov_mem_eax(data + 100);
            for (index, flags) in [0, 1, 0x101, 0x30101].into_iter().enumerate() {
                method(
                    &mut code,
                    data,
                    4,
                    &[4, callback, data + 68, flags],
                    data + 104 + index as u32 * 4,
                );
            }
            for (index, kind) in [0, 1, 2, 3].into_iter().enumerate() {
                method(
                    &mut code,
                    data,
                    4,
                    &[kind, callback, data + 68, 1],
                    data + 120 + index as u32 * 4,
                );
            }
            for (index, (kind, target, flags)) in [
                (5, callback, 1),
                (4, 0, 1),
                (4, data, 1),
                (4, callback, 2),
                (4, callback, 0x40000),
            ]
            .into_iter()
            .enumerate()
            {
                method(
                    &mut code,
                    data,
                    4,
                    &[kind, target, data + 68, flags],
                    data + 136 + index as u32 * 4,
                );
            }
            method(&mut code, data, 2, &[], data + 156);
            // The retired alias must not be accepted by enumeration.
            method(&mut code, data, 4, &[4, callback, data + 68, 1], data + 160);
            code.bytes.extend([0x89, 0xe0]);
            code.mov_mem_eax(data + 76);
            code.bytes.extend([0x31, 0xc0]);
            code.ret();
            m.cpu.write(code_address, &code.bytes).unwrap();
            if jit {
                let mut region = Region::new(code_address, code.bytes.len());
                region.reachable = true;
                m.cpu.executor_mut().add_region(region).unwrap();
                #[cfg(target_arch = "x86_64")]
                m.cpu.executor_mut().enable_auto_jit(false);
            }
            m.spawn("legacy controller enumeration", code_address, &[], 65536)
                .unwrap();
            let result = m.run_virtual(1_000_000);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            for offset in [100, 104, 108, 112, 116, 156] {
                assert_eq!(m.cpu.read_u32(data + offset).unwrap(), 0);
            }
            for offset in [120, 124, 128, 132] {
                assert_eq!(m.cpu.read_u32(data + offset).unwrap(), UNSUPPORTED);
            }
            for offset in [136, 140, 144, 148, 152, 160] {
                assert_eq!(m.cpu.read_u32(data + offset).unwrap(), INVALID);
            }
            assert_eq!(m.cpu.read_u32(data + 64).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(data + 68).unwrap(), 0x12345678);
            assert_eq!(
                m.cpu.read_u32(data + 72).unwrap(),
                m.cpu.read_u32(data + 76).unwrap()
            );
            assert_eq!(m.sys.objects.object_count(), before + 1); // only the guest thread
            assert_eq!(
                m.sys
                    .objects
                    .all_hosts::<adv_os::com::ComObject<Device>>()
                    .count(),
                0
            );
            assert_eq!(
                m.sys
                    .objects
                    .all_hosts::<adv_os::com::ComObject<LegacyFactory>>()
                    .count(),
                0
            );
            let diagnostics = m.ext.get::<adv_abi::ApiDiagnostics>();
            assert_eq!(diagnostics.stub_calls, 0);
            assert!(diagnostics.warnings.keys().any(|key| key.0
                == if wide {
                    "dinput.dll!IDirectInputW::EnumDevices"
                } else {
                    "dinput.dll!IDirectInputA::EnumDevices"
                }));
        }
    }
}

#[test]
fn sdk_legacy_slots_execute_through_both_guest_vtables_and_preserve_stack() {
    for jit in [false, true] {
        for wide in [false, true] {
            let (mut m, data) = fixture(jit);
            let before = m.sys.objects.object_count();
            let create = m
                .resolve_import(
                    "dinput.dll",
                    if wide {
                        "DirectInputCreateW"
                    } else {
                        "DirectInputCreateA"
                    },
                )
                .unwrap();
            m.cpu.write(data + 16, &KEYBOARD).unwrap();
            let fmt = keyboard_format(&mut m, data);
            m.ext.get_mut::<Win32>().user.keys[b'A' as usize] = 0x80;
            let code_address = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RX).unwrap();
            let mut code = Asm::new(code_address);
            code.bytes.extend([0x89, 0xe0]); // mov eax, esp
            code.mov_mem_eax(data + 128);
            for arg in [0, data, 0x500, 0x400000] {
                code.push_imm(arg);
            }
            code.call(create).mov_mem_eax(data + 100);
            method(&mut code, data, 3, &[data + 16, data + 4, 0], data + 104);
            method(&mut code, data + 4, 11, &[fmt], data + 108);
            method(&mut code, data + 4, 13, &[HWND, 6], data + 112);
            method(&mut code, data + 4, 7, &[], data + 116);
            method(&mut code, data + 4, 9, &[256, data + 3072], data + 120);
            method(&mut code, data + 4, 8, &[], data + 124);
            method(&mut code, data + 4, 2, &[], data + 136);
            method(&mut code, data, 2, &[], data + 140);
            code.bytes.extend([0x89, 0xe0]);
            code.mov_mem_eax(data + 132);
            code.bytes.extend([0x31, 0xc0]);
            code.ret();
            m.cpu.write(code_address, &code.bytes).unwrap();
            if jit {
                let mut region = Region::new(code_address, code.bytes.len());
                region.reachable = true;
                m.cpu.executor_mut().add_region(region).unwrap();
                #[cfg(target_arch = "x86_64")]
                m.cpu.executor_mut().enable_auto_jit(false);
            }
            m.spawn("legacy DirectInput guest", code_address, &[], 65536)
                .unwrap();
            let result = m.run_virtual(1_000_000);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            assert_eq!(
                m.cpu.read_u32(data + 128).unwrap(),
                m.cpu.read_u32(data + 132).unwrap()
            );
            for offset in [100, 104, 108, 112, 116, 120, 124, 136, 140] {
                assert_eq!(
                    m.cpu.read_u32(data + offset).unwrap(),
                    0,
                    "slot result {offset}"
                );
            }
            assert_eq!(m.cpu.read_u8(data + 3072 + 30).unwrap(), 0x80);
            assert_eq!(m.cpu.read_u8(data + 3072 + 31).unwrap(), 0);
            // The guest thread is the only additional common object after both COM releases.
            assert_eq!(m.sys.objects.object_count(), before + 1);
            let tables = m.ext.get::<State>().legacy.as_ref().unwrap();
            for (table, interface, slots) in [
                (
                    tables.input[usize::from(wide)],
                    if wide {
                        "IDirectInputW"
                    } else {
                        "IDirectInputA"
                    },
                    8,
                ),
                (
                    tables.device[usize::from(wide)],
                    if wide {
                        "IDirectInputDeviceW"
                    } else {
                        "IDirectInputDeviceA"
                    },
                    18,
                ),
            ] {
                assert_eq!(
                    {
                        let heap = m.ext.get_mut::<Heaps>();
                        heap.size(heap.process_heap, table)
                    },
                    Some(slots * 4)
                );
                for slot in 0..slots {
                    let entry = m.cpu.read_u32(table + slot * 4).unwrap();
                    assert!(m.sys.thunks.iter().any(
                        |(index, thunk)| adv_machine::layout::thunk_addr(index) == entry
                            && thunk.name.starts_with(interface)
                    ));
                }
            }
        }
    }
}

#[test]
fn shared_live_keyboard_and_optional_missing_objects_obey_the_requested_format() {
    let (mut m, data) = fixture(false);
    let (_, device) = make_device(&mut m, data, KEYBOARD, false);
    let fmt = keyboard_format(&mut m, data);
    configure(&mut m, device, fmt);
    assert_eq!(
        get_state(&mut env(&mut m), device, 256, data + 3072),
        NOT_ACQUIRED
    );
    assert_eq!(acquire(&mut env(&mut m), device), 0);
    let keys = std::sync::Arc::new(adv_sync::input::KeyState::default());
    keys.set(b'A' as u32, true);
    keys.set(0x10, true);
    m.ext.get_mut::<Win32>().user.host_keys = Some(keys.clone());
    assert_eq!(get_state(&mut env(&mut m), device, 256, data + 3072), 0);
    assert_eq!(m.cpu.read_u8(data + 3072 + 30).unwrap(), 0x80);
    assert_eq!(m.cpu.read_u8(data + 3072 + 42).unwrap(), 0x80);
    keys.set(0xa1, true);
    assert_eq!(get_state(&mut env(&mut m), device, 256, data + 3072), 0);
    assert_eq!(
        m.cpu.read_u8(data + 3072 + 42).unwrap(),
        0,
        "right Shift must not manufacture a left Shift press"
    );
    keys.set(0xa1, false);
    keys.set(b'A' as u32, false);
    assert_eq!(get_state(&mut env(&mut m), device, 256, data + 3072), 0);
    assert_eq!(m.cpu.read_u8(data + 3072 + 30).unwrap(), 0);
    assert_eq!(set_format(&mut env(&mut m), device, fmt), ACQUIRED);
    assert_eq!(unacquire(&mut env(&mut m), device), 0);
    // An optional instance zero has no key; a 256-byte buffer alone does not select all keys.
    let optional = format(&mut m, data, 256, &[(Some(KEY), 0, 0x8000000c)]);
    configure(&mut m, device, optional);
    assert_eq!(acquire(&mut env(&mut m), device), 0);
    assert_eq!(get_state(&mut env(&mut m), device, 256, data + 3072), 0);
    let mut result = [0xff; 256];
    m.cpu.read(data + 3072, &mut result).unwrap();
    assert_eq!(result, [0; 256]);
    m.ext.get_mut::<Win32>().user.top.clear();
    assert_eq!(
        get_state(&mut env(&mut m), device, 256, data + 3072),
        INPUT_LOST
    );
    assert_eq!(
        get_state(&mut env(&mut m), device, 256, data + 3072),
        NOT_ACQUIRED
    );
}

#[test]
fn relative_mouse_reads_validate_outputs_before_consuming_motion() {
    let (mut m, data) = fixture(false);
    let (_, device) = make_device(&mut m, data, MOUSE, true);
    let fmt = mouse_format(&mut m, data);
    configure(&mut m, device, fmt);
    let start = m.ext.get::<Win32>().user.cursor_pos;
    assert_eq!(acquire(&mut env(&mut m), device), 0);
    let user = &mut m.ext.get_mut::<Win32>().user;
    user.cursor_pos = (start.0 + 17, start.1 - 9);
    user.keys[1] = 0x80;
    post_input(&mut env(&mut m), 0x20a, 120 << 16, 0);
    m.cpu.protect(data + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(
        get_state(&mut env(&mut m), device, 16, data + 4096),
        INVALID
    );
    assert_eq!(
        get_state(&mut env(&mut m), device, 15, data + 3072),
        INVALID
    );
    assert_eq!(get_state(&mut env(&mut m), device, 16, data + 3072), 0);
    assert_eq!(m.cpu.read_u32(data + 3072).unwrap(), 17);
    assert_eq!(m.cpu.read_u32(data + 3076).unwrap() as i32, -9);
    assert_eq!(m.cpu.read_u32(data + 3080).unwrap(), 120);
    assert_eq!(m.cpu.read_u32(data + 3084).unwrap(), 0x80);
    assert_eq!(get_state(&mut env(&mut m), device, 16, data + 3072), 0);
    assert_eq!(m.cpu.read_u32(data + 3072).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(data + 3076).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(data + 3080).unwrap(), 0);
    assert_eq!(
        set_property(&mut env(&mut m), device, 1, data + 256),
        ACQUIRED
    );
}

#[test]
fn buffered_input_peek_overflow_notifications_and_release_share_common_objects() {
    let (mut m, data) = fixture(false);
    let (_, device) = make_device(&mut m, data, MOUSE, false);
    let fmt = mouse_format(&mut m, data);
    configure(&mut m, device, fmt);
    for (index, word) in [20, 16, 0, 0, 2].into_iter().enumerate() {
        m.cpu
            .write_u32(data + 192 + index as u32 * 4, word)
            .unwrap();
    }
    assert_eq!(set_property(&mut env(&mut m), device, 1, data + 192), 0);
    let event = m.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    assert_ne!(event, 0);
    assert_eq!(set_event(&mut env(&mut m), device, event), 0);
    let duplicate = super::device(&env(&mut m), device).unwrap().event;
    assert_ne!(duplicate, event);
    assert!(m.sys.objects.close(event).is_some());
    assert_eq!(acquire(&mut env(&mut m), device), 0);
    for (message, state) in [(0x201, 0x80), (0x202, 0), (0x201, 0x80)] {
        m.ext.get_mut::<Win32>().user.keys[1] = state;
        post_input(&mut env(&mut m), message, 0, 0);
    }
    assert!(matches!(
        m.sys.objects.get(duplicate),
        Some(Object::Event { signaled: true, .. })
    ));
    m.cpu.write_u32(data + 200, 99).unwrap();
    assert_eq!(
        get_data(&mut env(&mut m), device, 16, data + 3072, data + 200, 1),
        1
    );
    assert_eq!(m.cpu.read_u32(data + 200).unwrap(), 2);
    assert_eq!(m.cpu.read_u32(data + 3072).unwrap(), 12);
    assert_eq!(m.cpu.read_u32(data + 3076).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(data + 3092).unwrap(), 0x80);
    m.cpu.protect(data + 4096, 4096, Prot::READ).unwrap();
    m.cpu.write_u32(data + 200, 99).unwrap();
    assert_eq!(
        get_data(&mut env(&mut m), device, 16, data + 4096, data + 200, 0),
        INVALID
    );
    assert_eq!(m.cpu.read_u32(data + 200).unwrap(), 99);
    assert_eq!(
        get_data(&mut env(&mut m), device, 20, data + 3072, data + 200, 0),
        1
    );
    assert_eq!(m.cpu.read_u32(data + 200).unwrap(), 2);
    assert_eq!(m.cpu.read_u32(data + 3088).unwrap(), 0);
    m.cpu.write_u32(data + 200, 99).unwrap();
    assert_eq!(
        get_data(&mut env(&mut m), device, 16, data + 3072, data + 200, 0),
        0
    );
    assert_eq!(m.cpu.read_u32(data + 200).unwrap(), 0);
    assert_eq!(device_release_a(&mut env(&mut m), GuestPtr::new(device)), 0);
    assert!(
        m.sys.objects.get(duplicate).is_none(),
        "last COM Release closes its event alias"
    );
}

#[test]
fn unsupported_requests_and_a_w_identity_do_not_publish_partial_device_state() {
    let (mut m, data) = fixture(false);
    let (factory, device) = make_device(&mut m, data, KEYBOARD, false);
    m.cpu.write(data + 32, &IID_DEVICE_W).unwrap();
    assert_eq!(
        query_owned::<Device>(
            &mut env(&mut m),
            device,
            GuestPtr::new(data + 32),
            GuestPtr::new(data + 8)
        ),
        0
    );
    let wide = m.cpu.read_u32(data + 8).unwrap();
    m.cpu.write(data + 32, &adv_os::com::IUNKNOWN).unwrap();
    assert_eq!(
        query_owned::<Device>(
            &mut env(&mut m),
            wide,
            GuestPtr::new(data + 32),
            GuestPtr::new(data + 12)
        ),
        0
    );
    assert_eq!(m.cpu.read_u32(data + 12).unwrap(), device);
    assert_eq!(
        set_cooperative(&mut env(&mut m), wide, HWND, 5),
        UNSUPPORTED
    );
    assert!(
        super::device(&env(&mut m), device)
            .unwrap()
            .cooperative
            .is_none()
    );
    assert_eq!(set_property(&mut env(&mut m), device, 2, data), UNSUPPORTED);
    let fmt = keyboard_format(&mut m, data);
    m.cpu.write_u32(fmt + 8, 1).unwrap();
    assert_eq!(set_format(&mut env(&mut m), device, fmt), UNSUPPORTED);
    assert!(
        super::device(&env(&mut m), device)
            .unwrap()
            .format
            .is_none()
    );
    let fmt = keyboard_format(&mut m, data);
    m.cpu.write_u32(data + 512 + 4, 256).unwrap();
    assert_eq!(set_format(&mut env(&mut m), device, fmt), INVALID);
    assert!(
        super::device(&env(&mut m), device)
            .unwrap()
            .format
            .is_none()
    );
    let fmt = keyboard_format(&mut m, data);
    m.cpu.write_u32(data + 512 + 8, 0x1e08).unwrap();
    assert_eq!(set_format(&mut env(&mut m), device, fmt), UNSUPPORTED);
    let (mouse_factory, mouse) = make_device(&mut m, data, MOUSE, false);
    let fmt = mouse_format(&mut m, data);
    m.cpu.write_u32(data + 512 + 8, 0xffff02).unwrap();
    assert_eq!(set_format(&mut env(&mut m), mouse, fmt), UNSUPPORTED);
    assert!(super::device(&env(&mut m), mouse).unwrap().format.is_none());
    assert_eq!(device_release_a(&mut env(&mut m), GuestPtr::new(mouse)), 0);
    assert_eq!(
        input_release_a(&mut env(&mut m), GuestPtr::new(mouse_factory)),
        0
    );
    assert_eq!(device_release_w(&mut env(&mut m), GuestPtr::new(wide)), 2);
    assert_eq!(device_release_a(&mut env(&mut m), GuestPtr::new(device)), 1);
    assert_eq!(device_release_w(&mut env(&mut m), GuestPtr::new(wide)), 0);
    assert_eq!(input_release_a(&mut env(&mut m), GuestPtr::new(factory)), 0);
}

#[test]
fn queued_key_transitions_survive_newer_live_state_and_immediate_notification() {
    let (mut m, data) = fixture(false);
    let (_, device) = make_device(&mut m, data, KEYBOARD, false);
    let fmt = keyboard_format(&mut m, data);
    configure(&mut m, device, fmt);
    let live = std::sync::Arc::new(adv_sync::input::KeyState::default());
    // Hardware has already observed both transitions before the host drains its queue.
    live.set(b'A' as u32, true);
    live.set(b'A' as u32, false);
    m.ext.get_mut::<Win32>().user.host_keys = Some(live.clone());
    let event = m.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    assert_ne!(event, 0);
    assert_eq!(set_event(&mut env(&mut m), device, event), 0);
    assert_eq!(acquire(&mut env(&mut m), device), 0);
    post_input(&mut env(&mut m), 0x100, b'A' as u32, 0x1e0001);
    assert!(
        matches!(
            m.sys.objects.get(event),
            Some(Object::Event { signaled: true, .. })
        ),
        "notification also accompanies immediate state"
    );
    assert_eq!(get_state(&mut env(&mut m), device, 256, data + 3072), 0);
    assert_eq!(m.cpu.read_u8(data + 3072 + 30).unwrap(), 0);
    assert_eq!(unacquire(&mut env(&mut m), device), 0);
    for (index, word) in [20, 16, 0, 0, 4].into_iter().enumerate() {
        m.cpu
            .write_u32(data + 192 + index as u32 * 4, word)
            .unwrap();
    }
    assert_eq!(set_property(&mut env(&mut m), device, 1, data + 192), 0);
    assert_eq!(acquire(&mut env(&mut m), device), 0);
    post_input(&mut env(&mut m), 0x100, b'A' as u32, 0x1e0001);
    post_input(&mut env(&mut m), 0x101, b'A' as u32, 0xc01e0001);
    m.cpu.write_u32(data + 200, 4).unwrap();
    assert_eq!(
        get_data(&mut env(&mut m), device, 16, data + 3072, data + 200, 0),
        0
    );
    assert_eq!(m.cpu.read_u32(data + 200).unwrap(), 2);
    assert_eq!(m.cpu.read_u32(data + 3076).unwrap(), 0x80);
    assert_eq!(m.cpu.read_u32(data + 3092).unwrap(), 0);
    assert!(m.cpu.read_u32(data + 3084).unwrap() < m.cpu.read_u32(data + 3100).unwrap());
    let (_, background) = make_device(&mut m, data, KEYBOARD, true);
    assert_eq!(set_format(&mut env(&mut m), background, fmt), 0);
    assert_eq!(set_cooperative(&mut env(&mut m), background, HWND, 10), 0);
    assert_eq!(acquire(&mut env(&mut m), background), 0);
    live.set(b'A' as u32, true);
    assert_eq!(get_state(&mut env(&mut m), background, 256, data + 3072), 0);
    assert_eq!(m.cpu.read_u8(data + 3072 + 30).unwrap(), 0x80);
    live.release_all();
    release_input(&mut env(&mut m));
    assert_eq!(
        get_state(&mut env(&mut m), device, 256, data + 3072),
        NOT_ACQUIRED
    );
    assert_eq!(get_state(&mut env(&mut m), background, 256, data + 3072), 0);
    assert_eq!(m.cpu.read_u8(data + 3072 + 30).unwrap(), 0);
}
