//! The process image decides DEP under Windows' OptIn policy, and its header
//! supplies the version GetProcessVersion reports.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, PEB_ADDR, peb},
};
use adv_os::Process;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const BASE: u32 = 0x0100_0000;
// Unwritten image data reads back copy-on-write, never executable.
const PAGE_WRITECOPY: u32 = 8;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// A subsystem 5.1 executable whose only section is non-executable data
/// holding `mov eax, 37; ret`, as packers commonly leave their stubs.
fn exe(nx_compat: bool) -> Vec<u8> {
    let mut file = vec![0; 0x400];
    file[..2].copy_from_slice(b"MZ");
    put32(&mut file, 0x3c, 0x80);
    file[0x80..0x84].copy_from_slice(b"PE\0\0");
    put16(&mut file, 0x84, 0x14c);
    put16(&mut file, 0x86, 1);
    put16(&mut file, 0x94, 224);
    put16(&mut file, 0x96, 0x0103);
    let optional = 0x98;
    put16(&mut file, optional, 0x10b);
    put32(&mut file, optional + 16, 0x1010);
    put32(&mut file, optional + 28, BASE);
    put32(&mut file, optional + 32, 0x1000);
    put32(&mut file, optional + 36, 0x200);
    put16(&mut file, optional + 40, 5);
    put16(&mut file, optional + 48, 5);
    put16(&mut file, optional + 50, 1);
    put32(&mut file, optional + 56, 0x2000);
    put32(&mut file, optional + 60, 0x200);
    put16(&mut file, optional + 68, 2);
    put16(&mut file, optional + 70, if nx_compat { 0x100 } else { 0 });
    put32(&mut file, optional + 92, 16);
    let section = optional + 224;
    file[section..section + 5].copy_from_slice(b".data");
    put32(&mut file, section + 8, 0x1000);
    put32(&mut file, section + 12, 0x1000);
    put32(&mut file, section + 16, 0x200);
    put32(&mut file, section + 20, 0x200);
    put32(&mut file, section + 36, 0xc000_0040);
    file[0x210..0x216].copy_from_slice(&[0xb8, 37, 0, 0, 0, 0xc3]);
    file
}

struct Files(bool);
impl adv_os::FileSource for Files {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        path.eq_ignore_ascii_case("fixture.exe")
            .then(|| exe(self.0))
    }
}

fn machine(jit: bool, nx_compat: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/process-dep",
            "fixture.exe",
            "/nonexistent/process-dep-data",
        ),
    )
    .unwrap();
    let id = machine.load(&Files(nx_compat), "fixture.exe").unwrap();
    assert_eq!(adv_os::modules(&machine.ext).get(id).base, BASE);
    machine
        .cpu
        .write_u32(PEB_ADDR + peb::IMAGE_BASE, BASE)
        .unwrap();
    for address in [CODE, DATA] {
        assert!(machine.sys.vm.reserve_at(address, 0x1000));
    }
    machine.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x1000, Prot::RW).unwrap();
    machine
}

fn run(machine: &mut Machine<JitCpu>, mut code: Asm) -> RunResult {
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("process dep", CODE, &[], 0x10000).unwrap();
    machine.run_virtual(1_000_000)
}

#[test]
// spec: WIN32-IMAGE-6
fn image_without_nx_compat_executes_data_and_keeps_its_protection() {
    for jit in [false, true] {
        for nx_compat in [false, true] {
            let mut machine = machine(jit, nx_compat);
            let policy = machine
                .resolve_import("kernel32.dll", "GetProcessDEPPolicy")
                .unwrap();
            let query = machine
                .resolve_import("kernel32.dll", "VirtualQuery")
                .unwrap();
            let mut code = Asm::new(CODE);
            code.push_imm(DATA + 4)
                .push_imm(DATA)
                .push_imm(u32::MAX)
                .call(policy);
            code.push_imm(28)
                .push_imm(DATA + 0x20)
                .push_imm(BASE + 0x1000)
                .call(query);
            // The process exit code is the data-section stub's return value.
            code.call(BASE + 0x1010);
            let result = run(&mut machine, code);
            assert_eq!(
                matches!(result, RunResult::Exited(37)),
                !nx_compat,
                "jit {jit} nx {nx_compat}: {result:?}"
            );
            let flags = machine.cpu.read_u32(DATA).unwrap();
            let permanent = machine.cpu.read_u32(DATA + 4).unwrap();
            let expected = if nx_compat { (3, 1) } else { (0, 0) };
            assert_eq!((flags, permanent), expected, "jit {jit} nx {nx_compat}");
            let protect = machine.cpu.read_u32(DATA + 0x20 + 20).unwrap();
            assert_eq!(protect, PAGE_WRITECOPY);
        }
    }
}

#[test]
// spec: WIN32-IMAGE-7
fn process_version_comes_from_the_image_subsystem_version() {
    let mut machine = machine(false, true);
    let version = machine
        .resolve_import("kernel32.dll", "GetProcessVersion")
        .unwrap();
    let last_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0).call(version).mov_mem_eax(DATA);
    code.push_imm(0x7fff_fff0)
        .call(version)
        .mov_mem_eax(DATA + 4);
    code.call(last_error).mov_mem_eax(DATA + 8);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    let result = run(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let words: Vec<u32> = (0..3)
        .map(|i| machine.cpu.read_u32(DATA + i * 4).unwrap())
        .collect();
    assert_eq!(words, [0x0005_0001, 0, 87]);
}
