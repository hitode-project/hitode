use adv_machine::{Clock, Machine, Registry, RunResult};
use adv_win32::{Win32, Win32Config};
use pal_cpu::asm::Asm;
use pal_cpu::unicorn::UnicornCpu;
use pal_cpu::{Guest, GuestExt, Prot};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const LOG: u32 = DATA + 2048;
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as S, Value as V};
struct Test {
    machine: Machine<UnicornCpu>,
    code: Asm,
    next: u32,
}
impl Test {
    fn new() -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut machine = Machine::new(
            UnicornCpu::new(adv_machine::layout::PARK_ADDR).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(
                ".",
                "process-environment-fixture.exe",
                "target/process-environment-fixture",
            ),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            next: LOG,
        }
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        for arg in args.iter().rev() {
            match arg {
                V(v) => {
                    self.code.push_imm(*v);
                }
                S(p) => {
                    self.code.mov_eax_mem(*p).push_eax();
                }
            }
        }
        let thunk = self.machine.resolve_import("kernel32.dll", name).unwrap();
        let out = self.next;
        self.next += 4;
        self.code.call(thunk).mov_mem_eax(out);
        out
    }
    fn run(&mut self) {
        self.call("ExitProcess", &[V(0)]);
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.spawn("main", CODE, &[], 0x10000).unwrap();
        assert!(matches!(
            self.machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
    }
    fn value(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
    fn bytes(&self, address: u32, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        self.machine.cpu.read(address, &mut bytes).unwrap();
        bytes
    }
}

#[test]
fn environment_state_is_process_local_and_failed_updates_are_atomic() {
    let mut t = Test::new();
    t.machine
        .cpu
        .write(DATA, b"ADV_PRIVATE_ENV\0first\0bad=name\0")
        .unwrap();
    let created = t.call("SetEnvironmentVariableA", &[V(DATA), V(DATA + 16)]);
    let invalid = t.call("SetEnvironmentVariableA", &[V(DATA + 22), V(DATA + 16)]);
    let invalid_error = t.call("GetLastError", &[]);
    let retrieved = t.call("GetEnvironmentVariableA", &[V(DATA), V(DATA + 100), V(16)]);
    let snapshot = t.call("GetEnvironmentStringsW", &[]);
    let freed = t.call("FreeEnvironmentStringsW", &[S(snapshot)]);
    let twice = t.call("FreeEnvironmentStringsW", &[S(snapshot)]);
    let twice_error = t.call("GetLastError", &[]);
    t.run();
    assert_eq!(t.value(created), 1);
    assert_eq!((t.value(invalid), t.value(invalid_error)), (0, 87));
    assert_eq!(t.value(retrieved), 5);
    assert_eq!(t.bytes(DATA + 100, 6), b"first\0");
    assert_eq!(t.value(freed), 1);
    assert_eq!((t.value(twice), t.value(twice_error)), (0, 87));
    let mut other = Test::new();
    other.machine.cpu.write(DATA, b"ADV_PRIVATE_ENV\0").unwrap();
    let absent = other.call("GetEnvironmentVariableA", &[V(DATA), V(DATA + 100), V(16)]);
    let error = other.call("GetLastError", &[]);
    other.run();
    assert_eq!((other.value(absent), other.value(error)), (0, 203));
}

#[test]
fn invalid_version_sizes_and_readonly_environment_outputs_preserve_memory() {
    let mut t = Test::new();
    let readonly = DATA + 4096;
    t.machine.cpu.map(readonly, 4096, Prot::READ).unwrap();
    t.machine
        .cpu
        .write(DATA, b"ADV_PRIVATE_ENV\0first\0")
        .unwrap();
    t.machine.cpu.write_u32(DATA + 100, 0xfffffff0).unwrap();
    t.machine.cpu.write_u32(DATA + 104, 0x12345678).unwrap();
    t.call("SetEnvironmentVariableA", &[V(DATA), V(DATA + 16)]);
    let failed = t.call("GetEnvironmentVariableA", &[V(DATA), V(readonly), V(16)]);
    let error = t.call("GetLastError", &[]);
    let bad_version = t.call("GetVersionExW", &[V(DATA + 100)]);
    let version_error = t.call("GetLastError", &[]);
    t.run();
    assert_eq!((t.value(failed), t.value(error)), (0, 998));
    assert_eq!((t.value(bad_version), t.value(version_error)), (0, 87));
    assert_eq!(t.value(DATA + 104), 0x12345678);
}

#[test]
fn utf8_ansi_input_limit_counts_decoded_units_not_dbcs_bytes() {
    let mut t = Test::new();
    t.machine.ext.get_mut::<Win32>().codec = adv_win32::Codec::new(65001);
    let input = 0x500000;
    let output = 0x530000;
    t.machine.cpu.map(input, 0x20000, Prot::RW).unwrap();
    t.machine.cpu.map(output, 0x10000, Prot::RW).unwrap();
    let value = "中".repeat(24_000) + "\0";
    t.machine.cpu.write(input, value.as_bytes()).unwrap();
    t.machine.cpu.write(DATA, b"ADV_PRIVATE_ENV\0").unwrap();
    t.machine
        .cpu
        .write(
            DATA + 100,
            &"ADV_PRIVATE_ENV\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let set = t.call("SetEnvironmentVariableA", &[V(DATA), V(input)]);
    let size = t.call("GetEnvironmentVariableW", &[V(DATA + 100), V(0), V(0)]);
    let got = t.call(
        "GetEnvironmentVariableW",
        &[V(DATA + 100), V(output), V(24_001)],
    );
    t.run();
    assert_eq!(
        (t.value(set), t.value(size), t.value(got)),
        (1, 24_001, 24_000)
    );
    assert_eq!(t.machine.cpu.read_u16(output).unwrap(), 0x4e2d);
    assert_eq!(t.machine.cpu.read_u16(output + 47_998).unwrap(), 0x4e2d);
    assert_eq!(t.machine.cpu.read_u16(output + 48_000).unwrap(), 0);
}
