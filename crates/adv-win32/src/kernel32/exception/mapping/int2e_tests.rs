//! Native x86 direct-service capability failures, without service-number guesses.
use super::*;
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::{cell::RefCell, io::Write, rc::Rc};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

trait TestCpu: Backend {
    fn region(&mut self, _: usize) {}
}
impl TestCpu for UnicornCpu {}
impl TestCpu for JitCpu {
    fn region(&mut self, length: usize) {
        self.executor_mut()
            .add_region(Region::new(CODE, length))
            .unwrap();
    }
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    registry.add("test", "forbidden", Conv::Stdcall { argc: 0 }, |env| {
        env.cpu.write_u32(DATA + 0x200, 1).unwrap();
        Outcome::Return(0)
    });
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/sidt-agent/int2e-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
fn imm(code: &mut Asm, opcode: &[u8], value: u32) {
    code.bytes.extend_from_slice(opcode);
    code.bytes.extend_from_slice(&value.to_le_bytes());
}
struct Log(Rc<RefCell<Vec<u8>>>);
impl Write for Log {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn unsupported<B: TestCpu>(cpu: B, prefix: bool, selector: u32) {
    let mut machine = machine(cpu);
    let log = Rc::new(RefCell::new(Vec::new()));
    machine.sys.trace = adv_machine::trace::Trace::to_writer(Box::new(Log(log.clone())));
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    machine.resolve_import("test", "forbidden").unwrap();
    let token = machine
        .sys
        .thunks
        .iter()
        .find(|(_, thunk)| thunk.name == "forbidden")
        .unwrap()
        .0;
    let mut code = Asm::new(CODE);
    code.push_imm(0x1357_2468).call(set_error);
    for (opcode, value) in [
        (0xb8, selector),
        (0xba, 0x2468_1357), // Unmapped argument pointer must remain unread.
        (0xbb, 0x1357_2468),
        (0xb9, 0x3141_5926),
        (0xbd, 0x5566_7788),
        (0xbe, 0x2718_2818),
        (0xbf, 0x1122_3344),
    ] {
        imm(&mut code, &[opcode], value);
    }
    // A known API index is not a registered typed transport word. It must
    // remain raw even with unrelated stack operations before the interrupt.
    code.push_imm(token);
    code.bytes.push(0x90);
    imm(&mut code, &[0x89, 0x25], DATA);
    code.bytes.extend([0x9c, 0x8f, 0x05]);
    code.bytes.extend((DATA + 4).to_le_bytes());
    if prefix {
        code.bytes.push(0x66);
    }
    code.bytes.extend([0xcd, 0x2e]);
    let next = code.here();
    code.mov_mem_eax(DATA + 8);
    for (opcode, slot) in [
        (0x15, 12),
        (0x1d, 16),
        (0x0d, 20),
        (0x35, 24),
        (0x3d, 28),
        (0x25, 32),
    ] {
        imm(&mut code, &[0x89, opcode], DATA + slot);
    }
    code.bytes.extend([0x9c, 0x8f, 0x05]);
    code.bytes.extend((DATA + 36).to_le_bytes());
    code.add_esp(4).call(get_error).mov_mem_eax(DATA + 40);
    imm(&mut code, &[0x89, 0x2d], DATA + 44);
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.region(code.bytes.len());
    machine
        .spawn("authored unsupported direct service", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let rd = |slot| machine.cpu.read_u32(DATA + slot).unwrap();
    assert_eq!(rd(8), 0xc000_001c);
    assert_eq!(
        [rd(12), rd(16), rd(20), rd(24), rd(28)],
        [
            0x2468_1357,
            0x1357_2468,
            0x3141_5926,
            0x2718_2818,
            0x1122_3344
        ]
    );
    assert_eq!(rd(32), rd(0));
    assert_eq!(rd(36), rd(4));
    assert_eq!(rd(40), 0x1357_2468);
    assert_eq!(rd(44), 0x5566_7788);
    assert_eq!(rd(0x200), 0, "raw interrupt ran a host thunk");
    assert!(machine.sys.first_cpu_fault.is_none());
    assert!(machine.sys.latest_cpu_fault.is_none());
    let diagnostics = machine.ext.try_get::<adv_abi::ApiDiagnostics>().unwrap();
    assert_eq!(diagnostics.stub_calls, 0);
    assert_eq!(diagnostics.warnings.len(), 1);
    let ((api, reason), count) = diagnostics.warnings.iter().next().unwrap();
    assert_eq!(api, "native x86 INT 2E");
    assert!(reason.contains("no registered services"));
    assert_eq!(*count, 1);
    let output = String::from_utf8(log.borrow().clone()).unwrap();
    let raw: Vec<_> = output
        .lines()
        .filter(|line| line.starts_with("# raw_interrupt\t"))
        .collect();
    assert_eq!(raw.len(), 1, "host transport must not become raw trace");
    for field in [
        "\t2e\t".to_owned(),
        format!("Eax={selector:08x}"),
        "Edx=24681357".to_owned(),
        format!("Eip={next:08x}"),
        format!("Esp={:08x}", rd(0)),
        format!("Eflags={:08x}", rd(4)),
        format!("StackTop={token:08x}"),
        "Candidate=\"test\"!\"forbidden\"".to_owned(),
    ] {
        assert!(raw[0].contains(&field), "missing {field}: {}", raw[0]);
    }
}

#[test]
fn raw_int2e_native_x86_fails_without_guessing_services_or_touching_other_state() {
    for prefix in [false, true] {
        for selector in [0, 0xfff, 0x1000, 0x1fff, 0x4000_0000, 0x4000_1000] {
            unsupported(JitCpu::new(PARK_ADDR, false).unwrap(), prefix, selector);
            unsupported(JitCpu::new(PARK_ADDR, true).unwrap(), prefix, selector);
            unsupported(UnicornCpu::new(PARK_ADDR).unwrap(), prefix, selector);
        }
    }
}

#[test]
fn raw_int2e_validation_reads_only_executed_bytes() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    let next = CODE + 4096;
    machine.cpu.write(next - 2, &[0xcd, 0x2e]).unwrap();
    assert_eq!(raw_int2e_address(&machine.cpu, next).unwrap(), next - 2);
    machine.cpu.write(next - 2, &[0xcd, 0x80]).unwrap();
    assert!(raw_int2e_address(&machine.cpu, next).is_err());
    assert!(raw_int2e_address(&machine.cpu, 1).is_err());
}
