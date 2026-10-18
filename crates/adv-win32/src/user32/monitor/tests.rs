use super::*;
use crate::{Win32, Win32Config};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Reg},
};
use adv_machine::{Clock, Machine, Outcome, layout::PARK_ADDR};

fn fixture() -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in ["GetMonitorInfoA", "GetMonitorInfoW"] {
        assert_eq!(
            registry.get("user32.dll", name).unwrap().conv,
            adv_abi::Conv::Stdcall { argc: 2 }
        );
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/monitor-tests");
    Win32::install(
        &mut machine,
        Win32Config::new(&directory, "authored.exe", &directory),
    )
    .unwrap();
    machine.spawn("monitor", PARK_ADDR, &[], 65536).unwrap();
    machine.ext.get_mut::<Win32>().user.screen = (1280, 720);
    let buffer = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, buffer);
    (machine, buffer)
}
fn call(machine: &mut Machine<JitCpu>, stack: u32, output: u32, monitor: u32, wide: bool) -> u32 {
    for (index, value) in [0x12345678, monitor, output].into_iter().enumerate() {
        machine
            .cpu
            .write_u32(stack + index as u32 * 4, value)
            .unwrap();
    }
    let api = if wide { info_w_ABI } else { info_a_ABI };
    match (api.handler)(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }) {
        Outcome::Return(value) => value,
        _ => panic!("monitor query must return a BOOL"),
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
fn bytes(machine: &Machine<JitCpu>, output: u32) -> [u8; 128] {
    let mut bytes = [0; 128];
    machine.cpu.read(output, &mut bytes).unwrap();
    bytes
}

#[test]
fn base_and_ex_layouts_share_screen_workarea_and_device_name_without_overwriting_tail() {
    let (mut machine, stack) = fixture();
    let output = stack + 128;
    let count = machine.sys.objects.object_count();
    for (wide, size) in [(false, 40), (true, 40), (false, 72), (true, 104)] {
        machine.cpu.write(output, &[0x77; 128]).unwrap();
        machine.cpu.write_u32(output, size).unwrap();
        last_error(&mut machine, Some(0x76543210));
        assert_eq!(
            call(&mut machine, stack, output, window::HMONITOR_PRIMARY, wide),
            1
        );
        assert_eq!(last_error(&mut machine, None), 0x76543210);
        let raw = bytes(&machine, output);
        let words: Vec<_> = raw[..40]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_le_bytes(*bytes))
            .collect();
        assert_eq!(words, [size, 0, 0, 1280, 720, 0, 0, 1280, 680, 1]);
        if size != 40 {
            if wide {
                let name = raw[40..104]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|bytes| u16::from_le_bytes(*bytes))
                    .take_while(|unit| *unit != 0)
                    .collect::<Vec<_>>();
                assert_eq!(String::from_utf16(&name).unwrap(), r"\\.\DISPLAY1");
            } else {
                assert_eq!(&raw[40..53], b"\\\\.\\DISPLAY1\0");
            }
        }
        assert!(raw[size as usize..].iter().all(|&byte| byte == 0x77));
    }
    machine.ext.get_mut::<Win32>().user.screen = (640, 20);
    machine.cpu.write_u32(output, 40).unwrap();
    assert_eq!(
        call(&mut machine, stack, output, window::HMONITOR_PRIMARY, true),
        1
    );
    assert_eq!(machine.cpu.read_u32(output + 12).unwrap(), 640);
    assert_eq!(machine.cpu.read_u32(output + 32).unwrap(), 0);
    assert_eq!(machine.sys.objects.object_count(), count);
}

#[test]
fn wrong_sizes_handles_and_read_only_crossing_fail_without_partial_structure_writes() {
    let (mut machine, stack) = fixture();
    let output = stack + 128;
    for wide in [false, true] {
        for size in [0, 39, 41, if wide { 72 } else { 104 }, 108] {
            machine.cpu.write(output, &[0x77; 128]).unwrap();
            machine.cpu.write_u32(output, size).unwrap();
            let before = bytes(&machine, output);
            assert_eq!(
                call(&mut machine, stack, output, window::HMONITOR_PRIMARY, wide),
                0
            );
            assert_eq!(last_error(&mut machine, None), 87);
            assert_eq!(bytes(&machine, output), before);
        }
        machine
            .cpu
            .write_u32(output, if wide { 104 } else { 72 })
            .unwrap();
        let before = bytes(&machine, output);
        assert_eq!(call(&mut machine, stack, output, 0, wide), 0);
        assert_eq!(last_error(&mut machine, None), 1461);
        assert_eq!(bytes(&machine, output), before);
    }
    let crossing = stack + 4096 - 32;
    machine.cpu.write(crossing, &[0x77; 128]).unwrap();
    machine.cpu.write_u32(crossing, 104).unwrap();
    let before = bytes(&machine, crossing);
    machine.cpu.protect(stack + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(
        call(
            &mut machine,
            stack,
            crossing,
            window::HMONITOR_PRIMARY,
            true
        ),
        0
    );
    assert_eq!(last_error(&mut machine, None), 87);
    assert_eq!(bytes(&machine, crossing), before);
    assert_eq!(
        call(
            &mut machine,
            stack,
            0xdead0000,
            window::HMONITOR_PRIMARY,
            true
        ),
        0
    );
    assert_eq!(last_error(&mut machine, None), 87);
    assert!(machine.ext.try_get::<adv_abi::ApiDiagnostics>().is_none());
}
