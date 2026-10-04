use super::*;
use adv_cpu::{JitCpu, backend::Guest};
use adv_machine::{Clock, Machine};

fn fixture() -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(".", "user-profile.exe", "target/user-profile-tests"),
    )
    .unwrap();
    machine
        .spawn("profile", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let memory = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    (machine, memory)
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

#[test]
fn icon_font_fixed_a_w_layouts_ignore_size_hint_and_preserve_guard_bytes() {
    let (mut machine, memory) = fixture();
    for hint in [0, 59, 60, 61, 92, 128] {
        machine.cpu.write(memory, &[0xa5; 64]).unwrap();
        set_last_error(&mut env(&mut machine), 0x1234);
        assert_eq!(
            system_a(&mut env(&mut machine), 0x1f, hint, GuestPtr::new(memory), 0),
            1
        );
        assert_eq!(
            crate::util::last_error(&env(&mut machine)),
            if hint == 0 { 0 } else { 0x1234 }
        );
        let mut packed = [0; 64];
        machine.cpu.read(memory, &mut packed).unwrap();
        assert_eq!(i32::from_le_bytes(packed[..4].try_into().unwrap()), -12);
        assert_eq!(i32::from_le_bytes(packed[16..20].try_into().unwrap()), 400);
        assert_eq!(&packed[20..28], &[0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(&packed[28..41], b"MS UI Gothic\0");
        assert!(packed[41..60].iter().all(|&b| b == 0));
        assert_eq!(&packed[60..], &[0xa5; 4]);
    }
    machine.cpu.write(memory + 128, &[0xa5; 96]).unwrap();
    assert_eq!(
        system_w(
            &mut env(&mut machine),
            0x1f,
            92,
            GuestPtr::new(memory + 128),
            0
        ),
        1
    );
    let mut a = [0; 60];
    let mut w = [0; 96];
    machine.cpu.read(memory, &mut a).unwrap();
    machine.cpu.read(memory + 128, &mut w).unwrap();
    assert_eq!(&a[..28], &w[..28]);
    let units: Vec<_> = w[28..92]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    assert_eq!(
        &units[..13],
        &"MS UI Gothic\0".encode_utf16().collect::<Vec<_>>()
    );
    assert!(units[13..].iter().all(|&c| c == 0));
    assert_eq!(&w[92..], &[0xa5; 4]);
}

#[test]
fn icon_font_output_fault_and_unsupported_setting_preserve_all_guest_bytes() {
    let (mut machine, memory) = fixture();
    let output = memory + 4096 - 32;
    machine.cpu.write(output, &[0xa5; 92]).unwrap();
    machine
        .cpu
        .protect(memory + 4096, 4096, Prot::READ)
        .unwrap();
    assert_eq!(
        system_w(&mut env(&mut machine), 0x1f, 92, GuestPtr::new(output), 0),
        0
    );
    let mut packed = [0; 92];
    machine.cpu.read(output, &mut packed).unwrap();
    assert_eq!(packed, [0xa5; 92]);
    assert_eq!(crate::util::last_error(&env(&mut machine)), 87);
    assert_eq!(
        system_a(&mut env(&mut machine), 0x1f, 60, GuestPtr::new(0), 0),
        0
    );
    machine.cpu.write(memory, &[0xa5; 60]).unwrap();
    assert_eq!(
        system_a(&mut env(&mut machine), 0x22, 60, GuestPtr::new(memory), 3),
        0
    );
    let mut untouched = [0; 60];
    machine.cpu.read(memory, &mut untouched).unwrap();
    assert_eq!(untouched, [0xa5; 60]);
    assert_eq!(crate::util::last_error(&env(&mut machine)), 120);
}

#[test]
fn registered_messages_share_existing_user_names_across_a_w_and_clipboard_registration() {
    let (mut machine, memory) = fixture();
    machine.cpu.write(memory, b"commdlg_help\0").unwrap();
    let id = message_a(&mut env(&mut machine), AnsiStr(GuestPtr::new(memory)));
    assert_eq!(id, 0xc000);
    let name: Vec<_> = "COMMDLG_HELP\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(memory + 128, &name).unwrap();
    assert_eq!(
        message_w(&mut env(&mut machine), WideStr(GuestPtr::new(memory + 128))),
        id
    );
    assert_eq!(
        machine
            .ext
            .get_mut::<Win32>()
            .user
            .clipboard
            .register("commdlg_help".encode_utf16().collect()),
        Some(id)
    );
    let before = machine.sys.objects.object_count();
    machine.cpu.write(memory + 256, &[b'x'; 256]).unwrap();
    assert_eq!(
        message_a(&mut env(&mut machine), AnsiStr(GuestPtr::new(memory + 256))),
        0
    );
    assert_eq!(
        message_a(&mut env(&mut machine), AnsiStr(GuestPtr::new(0))),
        0
    );
    assert_eq!(machine.sys.objects.object_count(), before);
    // CP932 names and their UTF-16 spelling must resolve to the same USER ID.
    machine.cpu.write(memory + 512, &[0x82, 0xa0, 0]).unwrap();
    let japanese = message_a(&mut env(&mut machine), AnsiStr(GuestPtr::new(memory + 512)));
    machine
        .cpu
        .write(memory + 640, &[0x42, 0x30, 0, 0])
        .unwrap();
    assert_eq!(
        message_w(&mut env(&mut machine), WideStr(GuestPtr::new(memory + 640))),
        japanese
    );
}
