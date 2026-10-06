use super::*;
use crate::{Win32, Win32Config};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Reg},
};
use adv_machine::{Clock, Machine, Outcome, layout::PARK_ADDR};

const SENTINEL: u32 = 0x76543210;

fn fixture() -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in ["EnumDisplayDevicesA", "EnumDisplayDevicesW"] {
        let api = registry.get("user32.dll", name).unwrap();
        assert_eq!(api.conv, adv_abi::Conv::Stdcall { argc: 4 });
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/display-enumeration-tests");
    Win32::install(
        &mut machine,
        Win32Config::new(&directory, "authored.exe", &directory),
    )
    .unwrap();
    machine.spawn("display", PARK_ADDR, &[], 65536).unwrap();
    let stack = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, stack);
    (machine, stack)
}

fn call(
    machine: &mut Machine<JitCpu>,
    stack: u32,
    wide: bool,
    parent: u32,
    index: u32,
    output: u32,
    flags: u32,
) -> u32 {
    for (index, value) in [0x12345678, parent, index, output, flags]
        .into_iter()
        .enumerate()
    {
        machine
            .cpu
            .write_u32(stack + index as u32 * 4, value)
            .unwrap();
    }
    let api = if wide { devices_w_ABI } else { devices_a_ABI };
    match (api.handler)(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }) {
        Outcome::Return(value) => value,
        _ => panic!("display enumeration must return BOOL"),
    }
}

fn last_error(machine: &mut Machine<JitCpu>, value: Option<u32>) -> u32 {
    let mut env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    if let Some(value) = value {
        crate::util::set_last_error(&mut env, value);
    }
    crate::util::last_error(&env)
}
fn prepare(machine: &mut Machine<JitCpu>, output: u32, cb: u32) -> Vec<u8> {
    machine.cpu.write(output, &[0x77; 900]).unwrap();
    machine.cpu.write_u32(output, cb).unwrap();
    last_error(machine, Some(SENTINEL));
    raw(machine, output)
}
fn raw(machine: &Machine<JitCpu>, output: u32) -> Vec<u8> {
    let mut bytes = vec![0; 900];
    machine.cpu.read(output, &mut bytes).unwrap();
    bytes
}
fn text(bytes: &[u8], offset: usize, length: usize, wide: bool) -> String {
    if wide {
        let units = bytes[offset..offset + length * 2]
            .chunks_exact(2)
            .map(|word| u16::from_le_bytes(word.try_into().unwrap()))
            .take_while(|unit| *unit != 0)
            .collect::<Vec<_>>();
        String::from_utf16(&units).unwrap()
    } else {
        String::from_utf8(
            bytes[offset..offset + length]
                .iter()
                .copied()
                .take_while(|byte| *byte != 0)
                .collect(),
        )
        .unwrap()
    }
}
fn parent(machine: &mut Machine<JitCpu>, address: u32, value: &str, wide: bool) {
    let bytes = if wide {
        value
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    } else {
        value.bytes().chain([0]).collect()
    };
    machine.cpu.write(address, &bytes).unwrap();
}

#[test]
fn adapter_and_child_share_ansi_unicode_profile_without_hardware_identity_or_owner_changes() {
    let (mut machine, stack) = fixture();
    let output = stack + 128;
    let parent_address = stack + 1200;
    let owners = machine.sys.objects.object_count();
    for wide in [false, true] {
        let (size, label, state) = if wide { (840, 68, 324) } else { (424, 36, 164) };
        for flags in [0, 1] {
            prepare(&mut machine, output, size);
            assert_eq!(call(&mut machine, stack, wide, 0, 0, output, flags), 1);
            assert_eq!(last_error(&mut machine, None), SENTINEL);
            let bytes = raw(&machine, output);
            assert_eq!(text(&bytes, 4, 32, wide), PRIMARY_DEVICE_NAME);
            assert_eq!(
                text(&bytes, label, 128, wide),
                "adv-host virtual display adapter"
            );
            assert_eq!(
                u32::from_le_bytes(bytes[state..state + 4].try_into().unwrap()),
                5
            );
            assert!(
                bytes[state + 4..size as usize]
                    .iter()
                    .all(|byte| *byte == 0)
            );
            assert!(bytes[size as usize..].iter().all(|byte| *byte == 0x77));
        }
        parent(&mut machine, parent_address, r"\\.\display1", wide);
        prepare(&mut machine, output, size);
        assert_eq!(
            call(&mut machine, stack, wide, parent_address, 0, output, 0),
            1
        );
        let bytes = raw(&machine, output);
        assert_eq!(text(&bytes, 4, 32, wide), r"\\.\DISPLAY1\Monitor0");
        assert_eq!(text(&bytes, label, 128, wide), "adv-host virtual monitor");
        assert_eq!(
            u32::from_le_bytes(bytes[state..state + 4].try_into().unwrap()),
            3
        );
        assert_eq!(last_error(&mut machine, None), SENTINEL);
    }
    assert_eq!(machine.sys.objects.object_count(), owners);
    assert!(machine.ext.try_get::<adv_abi::ApiDiagnostics>().is_none());
}

#[test]
fn short_structures_copy_only_cb_and_exhaustion_clears_output_without_changing_last_error() {
    let (mut machine, stack) = fixture();
    let output = stack + 128;
    let parent_address = stack + 1200;
    for wide in [false, true] {
        let size = if wide { 840 } else { 424 };
        prepare(&mut machine, output, size);
        assert_eq!(call(&mut machine, stack, wide, 0, 0, output, 0), 1);
        let complete = raw(&machine, output);
        for cb in [0, 1, 3, 4, 5, 40, size - 1, size] {
            let mut expected = prepare(&mut machine, output, cb);
            let mut prefix = complete.clone();
            prefix[..4].copy_from_slice(&cb.to_le_bytes());
            expected[..cb as usize].copy_from_slice(&prefix[..cb as usize]);
            assert_eq!(call(&mut machine, stack, wide, 0, 0, output, 0), 1);
            assert_eq!(raw(&machine, output), expected);
            assert_eq!(last_error(&mut machine, None), SENTINEL);
        }
        for (name, index) in [
            (None, 1),
            (Some(PRIMARY_DEVICE_NAME), 1),
            (Some(""), 0),
            (Some(r"\\.\DISPLAY2"), 0),
        ] {
            let address = if let Some(name) = name {
                parent(&mut machine, parent_address, name, wide);
                parent_address
            } else {
                0
            };
            for cb in [40, size] {
                let mut expected = prepare(&mut machine, output, cb);
                expected[4..cb as usize].fill(0);
                assert_eq!(
                    call(&mut machine, stack, wide, address, index, output, 0),
                    0
                );
                assert_eq!(raw(&machine, output), expected);
                assert_eq!(last_error(&mut machine, None), SENTINEL);
            }
        }
    }
    assert!(machine.ext.try_get::<adv_abi::ApiDiagnostics>().is_none());
}

#[test]
fn invalid_buffers_are_atomic_and_unsupported_interface_queries_warn_without_fake_paths() {
    let (mut machine, stack) = fixture();
    let output = stack + 128;
    let parent_address = stack + 1200;
    for wide in [false, true] {
        let size = if wide { 840 } else { 424 };
        for cb in [size + 1, u32::MAX] {
            let before = prepare(&mut machine, output, cb);
            assert_eq!(call(&mut machine, stack, wide, 0, 0, output, 0), 0);
            assert_eq!(last_error(&mut machine, None), 87);
            assert_eq!(raw(&machine, output), before);
        }
        let before = prepare(&mut machine, output, size);
        assert_eq!(call(&mut machine, stack, wide, 0xdead0000, 0, output, 0), 0);
        assert_eq!(last_error(&mut machine, None), 87);
        assert_eq!(raw(&machine, output), before);
        assert_eq!(call(&mut machine, stack, wide, 0, 0, 0xdead0000, 0), 0);
        assert_eq!(last_error(&mut machine, None), 87);
        parent(&mut machine, parent_address, PRIMARY_DEVICE_NAME, wide);
        for flags in [1, 2] {
            let before = prepare(&mut machine, output, size);
            assert_eq!(
                call(&mut machine, stack, wide, parent_address, 0, output, flags),
                0
            );
            assert_eq!(last_error(&mut machine, None), 50);
            assert_eq!(raw(&machine, output), before);
        }
    }
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 4);
    assert!(diagnostics.warnings.values().all(|count| *count == 1));
    let crossing = stack + 4096 - 32;
    let before = prepare(&mut machine, crossing, 840);
    machine.cpu.protect(stack + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(call(&mut machine, stack, true, 0, 0, crossing, 0), 0);
    assert_eq!(last_error(&mut machine, None), 87);
    assert_eq!(raw(&machine, crossing), before);
    assert_eq!(
        machine.ext.get::<adv_abi::ApiDiagnostics>().warnings.len(),
        4
    );
}
