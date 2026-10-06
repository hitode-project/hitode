use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg},
};
use adv_machine::{Clock, Machine, layout};

#[test]
fn failed_initialization_url_and_close_have_defined_errors_without_touching_guest_strings() {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(
        JitCpu::new(layout::PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(".", "internet-profile.exe", "target/internet-profile-tests"),
    )
    .unwrap();
    machine
        .spawn("internet", layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, data);
    for (name, arguments, error) in [
        ("InternetOpenA", vec![1, 0, 1, 1, 0], 120),
        ("InternetOpenW", vec![1, 0, 1, 1, 0], 120),
        (
            "InternetOpenUrlA",
            vec![0, 1, 1, u32::MAX, 0xa0000002, 1],
            12172,
        ),
        (
            "InternetOpenUrlW",
            vec![0, 1, 1, u32::MAX, 0xa0000002, 1],
            12172,
        ),
        ("InternetCloseHandle", vec![0], 6),
        ("InternetCloseHandle", vec![1], 6),
    ] {
        let thunk = machine.resolve_import("wininet.dll", name).unwrap();
        let index = (thunk - layout::THUNK_BASE) / layout::THUNK_STRIDE;
        let handler = machine.sys.thunks.get(index).f.handler;
        machine.cpu.write_u32(data, 0x1234).unwrap();
        for (ordinal, argument) in arguments.iter().enumerate() {
            machine
                .cpu
                .write_u32(data + 4 + ordinal as u32 * 4, *argument)
                .unwrap();
        }
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: index,
        };
        let result = handler(&mut env);
        assert!(matches!(result, adv_machine::Outcome::Return(0)), "{name}");
        assert_eq!(crate::util::last_error(&env), error, "{name}");
    }
}
