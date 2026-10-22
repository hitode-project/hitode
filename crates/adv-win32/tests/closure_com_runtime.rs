//! COM initialization belongs to virtual threads; server counts belong to their process.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
fn call<B: Backend>(
    m: &mut Machine<B>,
    code: &mut Asm,
    dll: &str,
    name: &str,
    args: &[u32],
    out: u32,
) {
    let api = m.resolve_import(dll, name).unwrap();
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(api).mov_mem_eax(out);
}
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::ole32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(".", "fixture.exe", "target/closure-com-runtime-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let mut a = Asm::new(CODE);
    let mut expected = Vec::new();
    for (name, args, value) in [
        ("CoInitializeEx", vec![0, 2], 0),
        ("CoInitializeEx", vec![0, 2], 1),
        ("OleInitialize", vec![0], 1),
        ("OleInitialize", vec![0], 1),
        ("CoInitializeEx", vec![0, 0], 0x80010106),
        ("CoAddRefServerProcess", vec![], 1),
        ("CoAddRefServerProcess", vec![], 2),
        ("CoReleaseServerProcess", vec![], 1),
        ("CoReleaseServerProcess", vec![], 0),
        ("CoSuspendClassObjects", vec![], 0),
        ("CoResumeClassObjects", vec![], 0),
    ] {
        let out = DATA + expected.len() as u32 * 4;
        call(&mut m, &mut a, "ole32.dll", name, &args, out);
        expected.push((name, value));
    }
    for name in [
        "OleUninitialize",
        "OleUninitialize",
        "CoUninitialize",
        "CoUninitialize",
    ] {
        call(&mut m, &mut a, "ole32.dll", name, &[], DATA + 0x100);
    }
    call(
        &mut m,
        &mut a,
        "ole32.dll",
        "CoInitializeEx",
        &[0, 0],
        DATA + 0x104,
    );
    call(
        &mut m,
        &mut a,
        "ole32.dll",
        "OleInitialize",
        &[0],
        DATA + 0x108,
    );
    call(
        &mut m,
        &mut a,
        "ole32.dll",
        "CoUninitialize",
        &[],
        DATA + 0x100,
    );
    a.mov_mem_imm(DATA + 0x120, 0x55555555);
    call(
        &mut m,
        &mut a,
        "oleaut32.dll",
        "GetErrorInfo",
        &[0, DATA + 0x120],
        DATA + 0x124,
    );
    a.mov_mem_imm(DATA + 0x128, 0x55555555);
    call(
        &mut m,
        &mut a,
        "oleaut32.dll",
        "GetErrorInfo",
        &[1, DATA + 0x128],
        DATA + 0x12c,
    );
    let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
    a.push_imm(1).call(sleep);
    call(
        &mut m,
        &mut a,
        "ole32.dll",
        "CoInitializeEx",
        &[0, 2],
        DATA + 0x130,
    );
    a.mov_mem_imm(DATA + 0x200, 0)
        .mov_eax_mem(DATA + 0x200)
        .ret();
    m.cpu.write(CODE, &a.bytes).unwrap();
    let mut b = Asm::new(CODE + 0x1000);
    call(
        &mut m,
        &mut b,
        "ole32.dll",
        "CoInitializeEx",
        &[0, 0],
        DATA + 0x140,
    );
    b.push_imm(2).call(sleep);
    call(
        &mut m,
        &mut b,
        "ole32.dll",
        "CoInitializeEx",
        &[0, 2],
        DATA + 0x144,
    );
    b.mov_mem_imm(DATA + 0x204, 0)
        .mov_eax_mem(DATA + 0x204)
        .ret();
    m.cpu.write(CODE + 0x1000, &b.bytes).unwrap();
    m.spawn("COM STA", CODE, &[], 65536).unwrap();
    m.spawn("COM MTA", CODE + 0x1000, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, (name, value)) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 4).unwrap(),
            value,
            "{name} row {i}"
        );
    }
    for (offset, value) in [
        (0x104, 0),
        (0x108, 0x80010106),
        (0x120, 0),
        (0x124, 1),
        (0x128, 0x55555555),
        (0x12c, 0x80070057),
        (0x130, 0),
        (0x140, 0),
        (0x144, 0x80010106),
    ] {
        assert_eq!(
            m.cpu.read_u32(DATA + offset).unwrap(),
            value,
            "thread/error output {offset:x}"
        );
    }
}
#[test]
// spec: SYS-COM-1
fn com_and_ole_nesting_threads_server_counts_and_empty_error_slots_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
