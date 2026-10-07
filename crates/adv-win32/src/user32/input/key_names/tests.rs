use super::super::keymap_tests::{env, fixture};
use super::*;
use adv_cpu::backend::{Guest, GuestExt, Reg};
use adv_machine::Outcome;

#[test]
fn get_key_name_text_preserves_scan_flags_profile_codec_and_truncation_contracts() {
    let mut machine = fixture();
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for name in ["GetKeyNameTextA", "GetKeyNameTextW"] {
        assert!(registry.dynamic_available("user32.dll", name));
        let api = registry.get("user32.dll", name).unwrap();
        assert_eq!(api.conv, adv_abi::Conv::Stdcall { argc: 3 });
        for (i, value) in [0x004c0000, data + 128, 256].into_iter().enumerate() {
            machine
                .cpu
                .write_u32(data + 4 + i as u32 * 4, value)
                .unwrap();
        }
        machine.cpu.set_reg(Reg::Esp, data);
        assert!(matches!(
            (api.handler)(&mut env(&mut machine)),
            Outcome::Return(5)
        ));
    }
    for (profile, codepage) in [(0x0411_0411, 932), (0x0804_0804, 936), (0x0409_0409, 1252)] {
        let state = machine.ext.get_mut::<Win32>();
        state.user.keyboard_layout = profile;
        state.codec = adv_os::Codec::new(codepage);
        for (parameter, expected) in [
            (0x004c0000, "Num 5"),
            (0x00360000, "Right Shift"),
            (0x02360000, "Shift"),
            (
                0x011d0000,
                if profile == 0x0411_0411 {
                    "Right Control"
                } else {
                    "Right Ctrl"
                },
            ),
            (0x031d0000, "Ctrl"),
            (0x011c0000, "Num Enter"),
            (0x00470000, "Num 7"),
            (0x01470000, "Home"),
            (0xc04c1234_u32 as i32, "Num 5"),
        ] {
            for capacity in 1..=expected.len() as i32 + 2 {
                machine.cpu.write(data + 128, &[0xa5; 128]).unwrap();
                crate::util::set_last_error(&mut env(&mut machine), 0x12345678);
                let length = expected.len().min(capacity as usize - 1);
                assert_eq!(
                    get_a(
                        &mut env(&mut machine),
                        parameter,
                        AnsiStr(adv_abi::GuestPtr::new(data + 128)),
                        capacity
                    ),
                    length as i32
                );
                let mut raw = vec![0; length + 2];
                machine.cpu.read(data + 128, &mut raw).unwrap();
                assert_eq!(&raw[..length], &expected.as_bytes()[..length]);
                assert_eq!(&raw[length..], &[0, 0xa5]);
                assert_eq!(crate::util::last_error(&env(&mut machine)), 0x12345678);
                machine.cpu.write(data + 128, &[0xa5; 128]).unwrap();
                assert_eq!(
                    get_w(
                        &mut env(&mut machine),
                        parameter,
                        WideStr(adv_abi::GuestPtr::new(data + 128)),
                        capacity
                    ),
                    length as i32
                );
                let bytes: Vec<_> = expected[..length]
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect();
                let mut raw = vec![0; bytes.len() + 2];
                machine.cpu.read(data + 128, &mut raw).unwrap();
                assert_eq!(&raw[..bytes.len()], bytes);
                assert_eq!(&raw[bytes.len()..], &[0xa5; 2]);
            }
        }
    }
    machine.cpu.write(data + 128, &[0xa5; 16]).unwrap();
    assert_eq!(
        get_w(
            &mut env(&mut machine),
            0x00ff0000,
            WideStr(adv_abi::GuestPtr::new(data + 128)),
            8
        ),
        0
    );
    assert_eq!(machine.cpu.read_u32(data + 128).unwrap(), 0xa5a5a5a5);
    assert_eq!(
        get_a(
            &mut env(&mut machine),
            0x00ff0000,
            AnsiStr(adv_abi::GuestPtr::new(data + 128)),
            8
        ),
        0
    );
    assert_eq!(machine.cpu.read_u32(data + 128).unwrap(), 0xa5a5a500);
    machine.cpu.write(data + 128, &[0xa5; 16]).unwrap();
    assert_eq!(
        get_w(
            &mut env(&mut machine),
            0x004c0000,
            WideStr(adv_abi::GuestPtr::new(data + 128)),
            0
        ),
        0
    );
    assert_eq!(machine.cpu.read_u32(data + 128).unwrap(), 0xa5a5a5a5);
    assert_eq!(
        get_a(
            &mut env(&mut machine),
            0x004c0000,
            AnsiStr(adv_abi::GuestPtr::new(data + 128)),
            0
        ),
        0
    );
    assert_eq!(machine.cpu.read_u32(data + 128).unwrap(), 0xa5a5a500);
}

#[test]
fn get_key_name_text_bad_outputs_leave_memory_intact_and_unknown_profile_warns() {
    let mut machine = fixture();
    let pages = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.write(pages + 4092, &[0xa5; 4]).unwrap();
    machine.cpu.protect(pages + 4096, 4096, Prot(0)).unwrap();
    for output in [0, 1, u32::MAX - 3, pages + 4092] {
        assert_eq!(
            get_a(
                &mut env(&mut machine),
                0x004c0000,
                AnsiStr(adv_abi::GuestPtr::new(output)),
                256
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 998);
        assert_eq!(
            get_w(
                &mut env(&mut machine),
                0x004c0000,
                WideStr(adv_abi::GuestPtr::new(output)),
                256
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 998);
    }
    assert_eq!(machine.cpu.read_u32(pages + 4092).unwrap(), 0xa5a5a5a5);
    machine.cpu.protect(pages, 4096, Prot::READ).unwrap();
    assert_eq!(
        get_a(
            &mut env(&mut machine),
            0x004c0000,
            AnsiStr(adv_abi::GuestPtr::new(pages)),
            256
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 998);
    machine.cpu.protect(pages, 4096, Prot::RW).unwrap();
    assert_eq!(
        get_a(
            &mut env(&mut machine),
            0x004c0000,
            AnsiStr(adv_abi::GuestPtr::new(pages)),
            -1
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 8);
    assert_eq!(
        get_w(
            &mut env(&mut machine),
            0x004c0000,
            WideStr(adv_abi::GuestPtr::new(pages)),
            -1
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 998);
    assert_eq!(
        get_w(
            &mut env(&mut machine),
            0x004c0000,
            WideStr(adv_abi::GuestPtr::new(pages)),
            i32::MAX
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 998);
    machine.ext.get_mut::<Win32>().user.keyboard_layout = 0x0407_0407;
    assert_eq!(
        get_a(
            &mut env(&mut machine),
            0x004c0000,
            AnsiStr(adv_abi::GuestPtr::new(pages)),
            256
        ),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 50);
    assert_eq!(
        machine.ext.get::<adv_abi::ApiDiagnostics>().warnings.len(),
        1
    );
}
