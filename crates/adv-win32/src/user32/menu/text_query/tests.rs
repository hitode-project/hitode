use super::super::Item;
use super::*;
use adv_cpu::{JitCpu, backend::GuestExt};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

fn fixture(name: &str) -> (Machine<JitCpu>, u32, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for api in ["GetMenuStringA", "GetMenuStringW"] {
        assert!(registry.dynamic_available("user32.dll", api));
        assert_eq!(
            registry.get("user32.dll", api).unwrap().conv,
            adv_abi::Conv::Stdcall { argc: 5 }
        );
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/menu-text-tests")
        .join(name);
    let mut config = crate::Win32Config::new(&root, "fixture.exe", &root);
    config.os.codepage = 932;
    Win32::install(&mut machine, config).unwrap();
    machine.spawn("menu text", PARK_ADDR, &[], 65536).unwrap();
    let output = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let menus = &mut machine.ext.get_mut::<Win32>().user.menus;
    let menu = menus.new_menu(&mut machine.sys.objects).unwrap();
    let child = menus.new_menu(&mut machine.sys.objects).unwrap();
    menus
        .get_mut(&mut machine.sys.objects, menu)
        .unwrap()
        .push(Item {
            id: 1,
            text: "Root".encode_utf16().collect(),
            submenu: child,
            ..Default::default()
        });
    *menus.get_mut(&mut machine.sys.objects, child).unwrap() = vec![
        Item {
            id: 42,
            text: "A中B".encode_utf16().collect(),
            ..Default::default()
        },
        Item {
            id: 43,
            flags: 0x100,
            text: "Owner".encode_utf16().collect(),
            ..Default::default()
        },
    ];
    (machine, menu, output)
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

#[test]
fn menu_strings_share_recursive_items_guest_codepage_and_native_capacity_rules() {
    let (mut machine, menu, output) = fixture("strings");
    let mut e = env(&mut machine);
    set_last_error(&mut e, 0x4242);
    assert_eq!(query(&mut e, menu, 42, 0, 0, 0, false), 4);
    assert_eq!(query(&mut e, menu, 42, 0, 0, 0, true), 3);
    assert_eq!(query(&mut e, menu, 0, output, 20, 0x400, false), 4);
    assert_eq!(e.cpu.read_cstr(output, 20).unwrap(), b"Root");
    assert_eq!(query(&mut e, menu, 42, output, 20, 0, false), 4);
    assert_eq!(
        e.cpu.read_cstr(output, 20).unwrap(),
        [b'A', 0x92, 0x86, b'B']
    );
    for (capacity, length) in [(2, 1), (3, 1), (4, 3), (5, 4)] {
        assert_eq!(query(&mut e, menu, 42, output, capacity, 0, false), length);
        assert_eq!(
            e.cpu.read_cstr(output, 20).unwrap(),
            [b'A', 0x92, 0x86, b'B'][..length as usize]
        );
    }
    assert_eq!(query(&mut e, menu, 42, output, 3, 0, true), 2);
    assert_eq!(e.cpu.read_u16(output).unwrap(), u16::from(b'A'));
    assert_eq!(e.cpu.read_u16(output + 2).unwrap(), 0x4e2d);
    assert_eq!(e.cpu.read_u16(output + 4).unwrap(), 0);
    e.cpu.write(output, &[0xa5; 16]).unwrap();
    assert_eq!(query(&mut e, menu, 42, output, 0, 0, false), 4);
    assert_eq!(e.cpu.read_u32(output).unwrap(), 0xa5a5a5a5);
    assert_eq!(query(&mut e, menu, 42, output, 1, 0, false), 0);
    assert_eq!(e.cpu.read_u32(output).unwrap(), 0xa5a5a500);
    assert_eq!(query(&mut e, menu, 42, output, -1, 0, false), 4);
    assert_eq!(
        e.cpu.read_cstr(output, 20).unwrap(),
        [b'A', 0x92, 0x86, b'B']
    );
    e.cpu.write(output, &[0xa5; 16]).unwrap();
    assert_eq!(query(&mut e, menu, 42, output, -1, 0, true), 3);
    assert_eq!(e.cpu.read_u32(output).unwrap(), 0xa5a50000);
    assert_eq!(query(&mut e, menu, 43, output, 20, 0, false), 5);
    assert_eq!(e.cpu.read_cstr(output, 20).unwrap(), b"Owner");
    assert_eq!(crate::util::last_error(&e), 0x4242);
}

#[test]
fn menu_string_failures_clear_only_the_native_prefix_and_preserve_menu_state() {
    let (mut machine, menu, output) = fixture("failures");
    let mut e = env(&mut machine);
    for (handle, id, flags, error) in [
        (0, 42, 0, 87),
        (menu, 999, 0, 1456),
        (menu, 999, 0x400, 1456),
    ] {
        e.cpu.write(output, &[0xa5; 16]).unwrap();
        assert_eq!(query(&mut e, handle, id, output, 20, flags, true), 0);
        assert_eq!(crate::util::last_error(&e), error);
        assert_eq!(e.cpu.read_u32(output).unwrap(), 0xa5a50000);
        e.cpu.write(output, &[0xa5; 16]).unwrap();
        assert_eq!(query(&mut e, handle, id, output, 0, flags, false), 0);
        assert_eq!(e.cpu.read_u32(output).unwrap(), 0xa5a5a5a5);
    }
    assert_eq!(query(&mut e, menu, 42, 0, 1, 0, false), 0);
    assert_eq!(crate::util::last_error(&e), 998);
    e.cpu.write(output, &[0xa5; 16]).unwrap();
    e.cpu.protect(output, 4096, Prot::READ).unwrap();
    assert_eq!(query(&mut e, menu, 42, output, 20, 0, false), 0);
    assert_eq!(crate::util::last_error(&e), 998);
    assert_eq!(e.cpu.read_u32(output).unwrap(), 0xa5a5a5a5);
    assert_eq!(query(&mut e, menu, 42, 0, 0, 0, true), 3);
}
