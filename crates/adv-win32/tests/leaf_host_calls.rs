//! Authored API sequence compares the a64 leaf route with a full API trace.
#![cfg(target_arch = "aarch64")]
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, Trace, layout::PARK_ADDR};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn api(machine: &mut Machine<JitCpu>, code: &mut Asm, name: &str, args: &[u32], output: u32) {
    let address = machine.resolve_import("kernel32.dll", name).unwrap();
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(address).mov_mem_eax(DATA + output);
}

fn run(traced: bool) -> (Vec<u8>, u64, u64) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for (module, name) in [
        ("kernel32.dll", "GetTickCount"),
        ("kernel32.dll", "QueryPerformanceCounter"),
        ("winmm.dll", "timeGetTime"),
    ] {
        assert!(
            registry.get(module, name).unwrap().leaf.is_none(),
            "polling API can block"
        );
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, true).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/leaf-host-call-tests");
    adv_win32::Win32::install(
        &mut machine,
        adv_win32::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 0x4000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    machine
        .sys
        .thunks
        .set_guest_code(&mut machine.cpu, false)
        .unwrap();
    if traced {
        machine.sys.trace = Trace::to_writer(Box::new(std::io::sink()));
        machine.sys.trace.all = true;
    }
    let mut code = Asm::new(CODE);
    code.mov_ecx_imm(12);
    let again = code.here();
    code.push_ecx();
    api(&mut machine, &mut code, "SetLastError", &[0x12345678], 0);
    api(&mut machine, &mut code, "GetLastError", &[], 4);
    api(&mut machine, &mut code, "TlsSetValue", &[7, 0xabcdef01], 8);
    api(&mut machine, &mut code, "TlsGetValue", &[7], 12);
    api(&mut machine, &mut code, "GetLastError", &[], 16);
    api(&mut machine, &mut code, "GetCurrentThreadId", &[], 20);
    api(&mut machine, &mut code, "TlsSetValue", &[64, 99], 24);
    api(&mut machine, &mut code, "GetLastError", &[], 28);
    api(&mut machine, &mut code, "TlsGetValue", &[64], 32);
    api(&mut machine, &mut code, "GetLastError", &[], 36);
    code.pop_ecx().dec_ecx().jnz(again).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("API caller", CODE, &[], 65536).unwrap();
    assert!(matches!(machine.run(), RunResult::Exited(_)));
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 0x12345678);
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), 0xabcdef01);
    assert_eq!(machine.cpu.read_u32(DATA + 16).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 20).unwrap(), 0x1004);
    assert_eq!(machine.cpu.read_u32(DATA + 24).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 28).unwrap(), 87);
    assert_eq!(machine.cpu.read_u32(DATA + 32).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 36).unwrap(), 0);
    let mut output = vec![0; 40];
    machine.cpu.read(DATA, &mut output).unwrap();
    (output, machine.sys.stats.leaf_calls, machine.sys.host_calls)
}

// spec: A64-CTL-12
#[test]
fn registered_leaf_apis_match_their_traced_handlers() {
    let (leaf, leaves, calls) = run(false);
    let (traced, trace_leaves, trace_calls) = run(true);
    assert_eq!(leaf, traced);
    assert!(leaves > 0);
    assert_eq!(trace_leaves, 0);
    assert_eq!(calls, trace_calls);
}
