//! An authored API sequence gives the same results with and without a full API trace.
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
    api_from(machine, code, "kernel32.dll", name, args, output);
}

fn api_from(
    machine: &mut Machine<JitCpu>,
    code: &mut Asm,
    module: &str,
    name: &str,
    args: &[u32],
    output: u32,
) {
    let address = machine.resolve_import(module, name).unwrap();
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(address).mov_mem_eax(DATA + output);
}

fn run(traced: bool) -> (Vec<u8>, u64) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
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
    for (module, name, args, offset) in [
        ("kernel32.dll", "GetTickCount", vec![], 40),
        ("winmm.dll", "timeGetTime", vec![], 44),
        (
            "kernel32.dll",
            "QueryPerformanceCounter",
            vec![DATA + 128],
            48,
        ),
        (
            "kernel32.dll",
            "QueryPerformanceFrequency",
            vec![DATA + 136],
            52,
        ),
        ("user32.dll", "GetKeyState", vec![0x41], 56),
        ("user32.dll", "GetAsyncKeyState", vec![0x41], 60),
        ("user32.dll", "GetKeyboardState", vec![DATA + 256], 64),
        ("kernel32.dll", "IsDBCSLeadByte", vec![0x82], 68),
        ("kernel32.dll", "IsDBCSLeadByteEx", vec![932, 0x82], 72),
        ("user32.dll", "CharNextA", vec![DATA + 512], 76),
        ("user32.dll", "GetKeyboardState", vec![0], 80),
    ] {
        api_from(&mut machine, &mut code, module, name, &args, offset);
    }
    let desktop = machine.ext.get::<adv_win32::Win32>().user.desktop;
    for (module, name, args, output) in [
        ("user32.dll", "GetFocus", vec![], 96),
        ("user32.dll", "GetActiveWindow", vec![], 100),
        ("user32.dll", "GetSystemMetrics", vec![0], 104),
        ("user32.dll", "GetCursorPos", vec![DATA + 768], 108),
        ("user32.dll", "GetClipCursor", vec![DATA + 784], 112),
        (
            "user32.dll",
            "ScreenToClient",
            vec![desktop, DATA + 816],
            116,
        ),
        (
            "user32.dll",
            "GetWindowRect",
            vec![desktop, DATA + 832],
            120,
        ),
        ("kernel32.dll", "GetLocalTime", vec![DATA + 900], 124),
        ("kernel32.dll", "GetSystemTime", vec![DATA + 920], 148),
        (
            "kernel32.dll",
            "GetSystemTimeAsFileTime",
            vec![DATA + 944],
            152,
        ),
        ("kernel32.dll", "lstrlenA", vec![DATA + 512], 156),
        (
            "kernel32.dll",
            "lstrcpyA",
            vec![DATA + 960, DATA + 512],
            160,
        ),
        (
            "kernel32.dll",
            "lstrcmpA",
            vec![DATA + 960, DATA + 512],
            164,
        ),
    ] {
        api_from(&mut machine, &mut code, module, name, &args, output);
    }
    code.pop_ecx().dec_ecx().jnz(again);
    code.mov_ecx_imm(2050);
    let poll = code.here();
    code.push_ecx();
    api(&mut machine, &mut code, "GetTickCount", &[], 84);
    code.pop_ecx().dec_ecx().jnz(poll).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    let context = machine.spawn("API caller", CODE, &[], 65536).unwrap();
    let user = &mut machine.ext.get_mut::<adv_win32::Win32>().user;
    user.keys[0x41] = 0x80;
    user.queues.entry(context).or_default().keyboard.0[0x41] = 0x81;
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(_)), "{result:?}");
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 0x12345678);
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), 0xabcdef01);
    assert_eq!(machine.cpu.read_u32(DATA + 16).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 20).unwrap(), 0x1004);
    assert_eq!(machine.cpu.read_u32(DATA + 24).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 28).unwrap(), 87);
    assert_eq!(machine.cpu.read_u32(DATA + 32).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 36).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 56).unwrap(), 0xffff8001);
    assert_eq!(machine.cpu.read_u32(DATA + 60).unwrap(), 0x8000);
    assert_eq!(machine.cpu.read_u8(DATA + 256 + 0x41).unwrap(), 0x81);
    assert_eq!(machine.cpu.read_u32(DATA + 64).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 72).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 80).unwrap(), 0);
    let mut output = vec![0; 1024];
    machine.cpu.read(DATA, &mut output).unwrap();
    (output, machine.sys.host_calls)
}

// spec: A64-CTL-12
#[test]
fn direct_api_calls_match_their_traced_handlers() {
    let (direct, calls) = run(false);
    let (traced, trace_calls) = run(true);
    assert_eq!(direct, traced);
    assert_eq!(calls, trace_calls);
}
