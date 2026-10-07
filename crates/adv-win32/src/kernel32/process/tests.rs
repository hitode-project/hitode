//! The no-console profile has streams but no guest or host console HWND.
use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, Conv, ImplementationLevel, Registry};
use adv_cpu::JitCpu;
use adv_machine::{Clock, Env, Machine, Outcome, layout::PARK_ADDR};

#[test]
fn console_window_query_preserves_error_and_has_no_window_owner() {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let api = registry.get("kernel32.dll", "GetConsoleWindow").unwrap();
    assert_eq!(api.conv, Conv::Stdcall { argc: 0 });
    assert_eq!(
        registry.level("kernel32.dll", "GetConsoleWindow"),
        Some(ImplementationLevel::State)
    );
    assert!(registry.dynamic_available("kernel32.dll", "GetConsoleWindow"));
    let handler = api.handler;
    let cpu = JitCpu::new(PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/console",
            "fixture.exe",
            "/nonexistent/console-data",
        ),
    )
    .unwrap();
    machine.ext.get_or_default::<ApiDiagnostics>();
    machine
        .spawn("no-console query", PARK_ADDR, &[], 65536)
        .unwrap();
    let objects = machine.sys.objects.object_count();
    for error in [0, 0x1357_2468, u32::MAX] {
        let mut env = Env {
            context: 0,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        crate::util::set_last_error(&mut env, error);
        assert!(matches!(handler(&mut env), Outcome::Return(0)));
        assert_eq!(crate::util::last_error(&env), error);
        assert_eq!(env.sys.objects.object_count(), objects);
    }
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
}
