//! Authored WOW64 INT 2E boundaries, independent of guest service numbers.
use super::*;
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const HANDLER: u32 = CODE + 512;
const DATA: u32 = 0x410000;

trait TestCpu: Backend {
    fn regions(&mut self, _: usize, _: usize) {}
}
impl TestCpu for UnicornCpu {}
impl TestCpu for JitCpu {
    fn regions(&mut self, main: usize, handler: usize) {
        self.executor_mut()
            .add_region(Region::new(CODE, main))
            .unwrap();
        if handler != 0 {
            self.executor_mut()
                .add_region(Region::new(HANDLER, handler))
                .unwrap();
        }
    }
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
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
fn store(code: &mut Asm, opcode: &[u8], address: u32, value: u32) {
    imm(code, opcode, address);
    code.bytes.extend_from_slice(&value.to_le_bytes());
}

fn handled<B: TestCpu>(cpu: B, prefix: bool, vectored: bool) {
    let mut machine = machine(cpu);
    let add = machine
        .resolve_import("kernel32.dll", "AddVectoredExceptionHandler")
        .unwrap();
    let mut code = Asm::new(CODE);
    if vectored {
        code.push_imm(HANDLER).push_imm(1).call(add);
    } else {
        code.push_imm(HANDLER);
        code.bytes.extend([0x64, 0xff, 0x35, 0, 0, 0, 0]);
        code.bytes.extend([0x64, 0x89, 0x25, 0, 0, 0, 0]);
    }
    imm(&mut code, &[0xb8], u32::MAX);
    imm(&mut code, &[0xba], 0x2468_1357); // Unmapped arguments must not be dereferenced.
    imm(&mut code, &[0xbb], 0x1357_2468);
    imm(&mut code, &[0x89, 0x25], DATA + 0x100);
    code.bytes.extend([0x9c, 0x8f, 0x05]);
    code.bytes.extend((DATA + 0x104).to_le_bytes());
    if prefix {
        code.bytes.push(0x66);
    }
    let fault = code.here();
    code.bytes.extend([0xcd, 0x2e]);
    let next = code.here();
    code.bytes.extend([0x9c, 0x8f, 0x05]);
    code.bytes.extend((DATA + 0x108).to_le_bytes());
    code.mov_mem_eax(DATA + 0x110);
    imm(&mut code, &[0x89, 0x15], DATA + 0x114);
    imm(&mut code, &[0x89, 0x1d], DATA + 0x118);
    store(&mut code, &[0xc7, 0x05], DATA + 0x11c, 1);
    if !vectored {
        code.bytes.extend([0x58, 0x64, 0xa3, 0, 0, 0, 0]);
        code.add_esp(4);
    }
    code.bytes.extend([0x31, 0xc0]);
    code.ret();

    let mut handler = Asm::new(HANDLER);
    if vectored {
        handler
            .bytes
            .extend([0x8b, 0x44, 0x24, 4, 0x8b, 8, 0x8b, 0x50, 4]);
    } else {
        handler
            .bytes
            .extend([0x8b, 0x4c, 0x24, 4, 0x8b, 0x54, 0x24, 12]);
    } // ECX=record, EDX=context.
    for (slot, offset) in [0_u8, 4, 12, 16].into_iter().enumerate() {
        handler.bytes.extend([0x8b, 0x41, offset]);
        handler.mov_mem_eax(DATA + slot as u32 * 4);
    }
    for (slot, offset) in [0xb8_u32, 0xb0, 0xa8, 0xa4, 0xc4, 0xc0]
        .into_iter()
        .enumerate()
    {
        imm(&mut handler, &[0x8b, 0x82], offset);
        handler.mov_mem_eax(DATA + 16 + slot as u32 * 4);
    }
    imm(&mut handler, &[0xff, 0x05], DATA + 48);
    // The first continue leaves EIP unchanged and must repeat the exception.
    handler.bytes.extend([0x83, 0x3d]);
    handler.bytes.extend((DATA + 48).to_le_bytes());
    handler.bytes.extend([1, 0x74, 10]);
    store(&mut handler, &[0xc7, 0x82], 0xb8, next);
    imm(&mut handler, &[0xbb], 0xdead_beef);
    imm(&mut handler, &[0xb8], if vectored { u32::MAX } else { 0 });
    handler.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.write(HANDLER, &handler.bytes).unwrap();
    machine.cpu.regions(code.bytes.len(), handler.bytes.len());
    machine
        .spawn("authored raw system-service interrupt", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let rd = |slot| machine.cpu.read_u32(DATA + slot).unwrap();
    assert_eq!(
        [rd(0), rd(4), rd(8), rd(12)],
        [ILLEGAL_INSTRUCTION, 0, fault, 0]
    );
    assert_eq!(
        [rd(16), rd(20), rd(24), rd(28)],
        [fault, u32::MAX, 0x2468_1357, 0x1357_2468]
    );
    assert_eq!(rd(32), rd(0x100));
    assert_eq!(rd(36), rd(0x104));
    assert_eq!(rd(48), 2);
    assert_eq!(rd(0x108), rd(0x104));
    assert_eq!(
        [rd(0x110), rd(0x114), rd(0x118), rd(0x11c)],
        [u32::MAX, 0x2468_1357, 0x1357_2468, 1]
    );
    assert!(machine.sys.first_cpu_fault.is_none());
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
fn raw_int2e_uses_wow64_illegal_instruction_context_in_veh_and_seh() {
    for prefix in [false, true] {
        for vectored in [false, true] {
            handled(JitCpu::new(PARK_ADDR, false).unwrap(), prefix, vectored);
            handled(JitCpu::new(PARK_ADDR, true).unwrap(), prefix, vectored);
            handled(UnicornCpu::new(PARK_ADDR).unwrap(), prefix, vectored);
        }
    }
}

fn unhandled<B: TestCpu>(cpu: B) {
    let mut machine = machine(cpu);
    let mut code = Asm::new(CODE);
    imm(&mut code, &[0xb8], u32::MAX);
    let fault = code.here();
    code.bytes.extend([0xcd, 0x2e]);
    let next = code.here();
    store(&mut code, &[0xc7, 0x05], DATA, 1);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.regions(code.bytes.len(), 0);
    machine
        .spawn("unhandled authored INT 2E", CODE, &[], 65536)
        .unwrap();
    let RunResult::Failed(error) = machine.run_virtual(1_000_000) else {
        panic!("unhandled illegal instruction must not continue")
    };
    assert!(
        error.message.contains(&format!(
            "unhandled Windows exception 0xc000001d at {fault:#010x}"
        )),
        "{}",
        error.message
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    let snapshot = machine.sys.first_cpu_fault.as_ref().unwrap();
    assert_eq!(snapshot.fault.pc, next);
    assert!(
        snapshot
            .registers
            .contains(&(adv_cpu::backend::Reg::Eax, u32::MAX))
    );
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
fn unhandled_raw_int2e_keeps_the_original_stop_and_does_not_skip_execution() {
    unhandled(JitCpu::new(PARK_ADDR, false).unwrap());
    unhandled(JitCpu::new(PARK_ADDR, true).unwrap());
    unhandled(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn raw_int2e_mapping_checks_executed_bytes_and_reads_only_the_instruction() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut state = machine.cpu.capture_architecture().unwrap();
    state.eip = CODE + 4096;
    machine.cpu.write(state.eip - 2, &[0xcd, 0x2e]).unwrap();
    let event = Exception::Trap {
        vector: 0x2e,
        pc: state.eip,
    };
    let record = map(&machine.cpu, &event, &state).unwrap();
    assert_eq!(record.code, ILLEGAL_INSTRUCTION);
    assert_eq!(record.address, state.eip - 2);
    assert_eq!((record.flags, record.count), (0, 0));
    normalize_context(&record, &mut state);
    assert_eq!(state.eip, CODE + 4094);
    state.eip = CODE + 4096;
    machine.cpu.write(state.eip - 2, &[0xcd, 0x80]).unwrap();
    assert!(map(&machine.cpu, &event, &state).is_err());
}
