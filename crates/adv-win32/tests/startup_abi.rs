//! Faulting typed startup calls must validate outputs before changing guest state.
use adv_abi::{ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, FpuEnvironment, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;

fn registry() -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    registry
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut machine = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/startup-abi",
            "fixture.exe",
            "/nonexistent/startup-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine
}
fn failed<B: Backend>(machine: &mut Machine<B>, module: &str, name: &str, args: &[u32]) -> String {
    let address = machine.resolve_import(module, name).unwrap();
    let mut code = Asm::new(CODE);
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(address).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("protected startup output", CODE, &[], 0x10000)
        .unwrap();
    let RunResult::Failed(failure) = machine.run_virtual(1_000_000) else {
        panic!("invalid output or parameter was accepted");
    };
    failure.message
}
fn protected_path<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let input: Vec<_> = "C:\\Game\\leaf.txt"
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(DATA, &input).unwrap();
    let sentinel = [0x77; 128];
    machine.cpu.write(DATA + 256, &sentinel).unwrap();
    assert!(
        failed(
            &mut machine,
            "kernel32.dll",
            "GetFullPathNameW",
            &[DATA, 128, DATA + 256, READ_ONLY]
        )
        .contains("ABI guest memory fault")
    );
    let mut actual = [0; 128];
    machine.cpu.read(DATA + 256, &mut actual).unwrap();
    assert_eq!(actual, sentinel);
}
fn protected_affinity<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    machine.cpu.write_u32(DATA, 0x12345678).unwrap();
    assert!(
        failed(
            &mut machine,
            "kernel32.dll",
            "GetProcessAffinityMask",
            &[u32::MAX, DATA, READ_ONLY]
        )
        .contains("ABI guest memory fault")
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0x12345678);
}
fn protected_floating<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let environment = FpuEnvironment {
        x87_control: 0x027f,
        mxcsr: 0x1f80,
    };
    machine.cpu.set_fpu_environment(environment).unwrap();
    assert!(
        failed(
            &mut machine,
            "msvcrt.dll",
            "_controlfp_s",
            &[READ_ONLY, 0x100, 0x300]
        )
        .contains("_controlfp_s")
    );
    assert_eq!(machine.cpu.fpu_environment().unwrap(), environment);
}
fn no_handler<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let environment = FpuEnvironment {
        x87_control: 0x027f,
        mxcsr: 0x1f80,
    };
    machine.cpu.set_fpu_environment(environment).unwrap();
    assert!(
        failed(
            &mut machine,
            "ucrtbase.dll",
            "_controlfp_s",
            &[DATA, 0x80000000, 0x80000000]
        )
        .contains("invalid parameter without a returning guest handler")
    );
    assert_eq!(machine.cpu.fpu_environment().unwrap(), environment);
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0x9001f);
}
#[test]
fn full_path_validates_filepart_before_writing_the_path() {
    for jit in [false, true] {
        protected_path(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    protected_path(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
fn affinity_validates_both_outputs_before_writing_the_first() {
    for jit in [false, true] {
        protected_affinity(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    protected_affinity(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
fn protected_control_output_leaves_the_fpu_environment_unchanged() {
    for jit in [false, true] {
        protected_floating(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    protected_floating(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
fn invalid_control_without_a_handler_fails_explicitly() {
    for jit in [false, true] {
        no_handler(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    no_handler(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
fn startup_handlers_have_generated_typed_implementations() {
    let registry = registry();
    for (module, names) in [
        (
            "kernel32.dll",
            &[
                "GetFullPathNameW",
                "GetFullPathNameA",
                "GetTempPathW",
                "GetTempPathA",
                "GetCurrentDirectoryW",
                "GetCurrentDirectoryA",
                "SetCurrentDirectoryW",
                "SetCurrentDirectoryA",
                "CreateDirectoryW",
                "CreateDirectoryA",
                "RemoveDirectoryW",
                "RemoveDirectoryA",
                "CopyFileW",
                "CopyFileA",
                "MoveFileW",
                "MoveFileA",
                "DeleteFileW",
                "DeleteFileA",
                "GetProcessAffinityMask",
                "SetProcessAffinityMask",
                "SetThreadAffinityMask",
                "GetSystemInfo",
                "GetNativeSystemInfo",
                "GetCurrentProcessorNumber",
            ][..],
        ),
        (
            "msvcrt.dll",
            &["_controlfp", "_control87", "_controlfp_s", "_errno"][..],
        ),
        (
            "msvcr80.dll",
            &[
                "_controlfp",
                "_control87",
                "_controlfp_s",
                "__control87_2",
                "_set_invalid_parameter_handler",
                "_get_invalid_parameter_handler",
                "_errno",
            ][..],
        ),
        (
            "msvcr90.dll",
            &[
                "_controlfp",
                "_control87",
                "_controlfp_s",
                "__control87_2",
                "_set_invalid_parameter_handler",
                "_get_invalid_parameter_handler",
                "_errno",
            ][..],
        ),
        (
            "ucrtbase.dll",
            &[
                "_controlfp",
                "_control87",
                "_controlfp_s",
                "__control87_2",
                "_set_invalid_parameter_handler",
                "_get_invalid_parameter_handler",
                "_errno",
            ][..],
        ),
    ] {
        for name in names {
            assert_eq!(
                registry.level(module, name),
                Some(ImplementationLevel::Implement),
                "{module}!{name}"
            );
            assert!(
                !registry
                    .signature_conflicts()
                    .iter()
                    .any(|c| c.module == module && c.name == *name)
            );
            assert!(
                !registry
                    .missing_signatures()
                    .iter()
                    .any(|s| s == &format!("{module}!{name}"))
            );
        }
    }
}
