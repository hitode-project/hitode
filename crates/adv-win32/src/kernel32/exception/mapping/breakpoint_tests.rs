//! Real CC/CD03 guest interruption and explicit CONTEXT continuation tests.
use super::*;
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Env, Machine, Outcome, RunResult, exception::Checkpoint, layout::PARK_ADDR,
};
use adv_unicorn::UnicornCpu;
use std::{cell::RefCell, io::Write, rc::Rc};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const HANDLER: u32 = CODE + 512;
const ROWS: u32 = DATA + 32;
const RAW: u32 = DATA + 256;

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
        .join("../../target/breakpoint-exception-tests");
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

// Record the real backend boundary before the Win32 adapter adjusts its saved
// image. This observes live guest execution, not an injected synthetic Trap.
fn observe_stop(env: &mut Env<'_>, event: Exception, checkpoint: Checkpoint) -> Outcome {
    let Exception::Trap { vector: 3, pc } = event else {
        panic!("expected real INT3")
    };
    let row = env.cpu.read_u32(DATA).unwrap();
    assert!(row < 2);
    env.cpu.write_u32(RAW + row * 8, pc).unwrap();
    env.cpu
        .write_u32(RAW + row * 8 + 4, checkpoint.state.eip)
        .unwrap();
    super::super::cpu_exception(env, event, checkpoint)
}

fn handled<B: TestCpu>(cpu: B, long: bool, vectored: bool) {
    let mut machine = machine(cpu);
    machine.sys.exception_handler = Some(observe_stop);
    let add = machine
        .resolve_import("kernel32.dll", "AddVectoredExceptionHandler")
        .unwrap();
    let mut code = Asm::new(CODE);
    if vectored {
        code.push_imm(HANDLER).push_imm(1).call(add);
    } else {
        code.push_imm(HANDLER);
        code.bytes
            .extend_from_slice(&[0x64, 0xff, 0x35, 0, 0, 0, 0]);
        code.bytes
            .extend_from_slice(&[0x64, 0x89, 0x25, 0, 0, 0, 0]);
    }
    imm(&mut code, &[0xb8], 0x12345678);
    imm(&mut code, &[0xbb], 0xaabbccdd);
    imm(&mut code, &[0x89, 0x25], DATA + 280);
    code.bytes.extend_from_slice(&[0x9c, 0x8f, 0x05]);
    code.bytes.extend_from_slice(&(DATA + 284).to_le_bytes());
    let start = code.here();
    code.bytes
        .extend_from_slice(if long { &[0xcd, 0x03][..] } else { &[0xcc][..] });
    let next = code.here();
    code.bytes.push(0x40); // inc eax: must run exactly once after explicit CONTEXT edit.
    code.mov_mem_eax(DATA + 4);
    imm(&mut code, &[0x89, 0x1d], DATA + 8);
    if !vectored {
        code.bytes
            .extend_from_slice(&[0x58, 0x64, 0xa3, 0, 0, 0, 0]);
        code.add_esp(4);
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();

    let mut handler = Asm::new(HANDLER);
    if vectored {
        handler
            .bytes
            .extend_from_slice(&[0x8b, 0x44, 0x24, 0x04, 0x8b, 0x08, 0x8b, 0x50, 0x04]);
    } else {
        handler
            .bytes
            .extend_from_slice(&[0x8b, 0x4c, 0x24, 0x04, 0x8b, 0x54, 0x24, 0x0c]);
    } // ECX=record, EDX=context.
    imm(&mut handler, &[0x8b, 0x3d], DATA);
    handler.bytes.extend_from_slice(&[0xc1, 0xe7, 0x06]); // edi = callback index *64.
    for (slot, offset) in [0_u8, 4, 12, 16, 20].into_iter().enumerate() {
        handler.bytes.extend_from_slice(&[0x8b, 0x41, offset]);
        imm(&mut handler, &[0x89, 0x87], ROWS + slot as u32 * 4);
    }
    for (slot, offset) in [0xb8_u32, 0xb0, 0xa4, 0xc4, 0xc0].into_iter().enumerate() {
        imm(&mut handler, &[0x8b, 0x82], offset);
        imm(&mut handler, &[0x89, 0x87], ROWS + 20 + slot as u32 * 4);
    }
    imm(&mut handler, &[0xff, 0x05], DATA);
    if !long {
        // Like the native oracle: leave the first CC context unchanged. The
        // dispatcher must not silently turn continue into an automatic skip.
        handler.bytes.extend_from_slice(&[0x83, 0x3d]);
        handler.bytes.extend_from_slice(&DATA.to_le_bytes());
        handler.bytes.extend_from_slice(&[1, 0x74, 7]); // je past the next 7-byte instruction.
    }
    handler
        .bytes
        .extend_from_slice(&[0x83, 0x82, 0xb8, 0, 0, 0, 1]); // add CONTEXT.Eip,1.
    imm(&mut handler, &[0xbb], 0xdeadbeef);
    imm(&mut handler, &[0xb8], if vectored { u32::MAX } else { 0 });
    handler.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.write(HANDLER, &handler.bytes).unwrap();
    machine.cpu.regions(code.bytes.len(), handler.bytes.len());
    machine
        .spawn("native INT3 CONTEXT continuation", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let rd = |at| machine.cpu.read_u32(at).unwrap();
    let calls = if long { 1 } else { 2 };
    assert_eq!(rd(DATA), calls);
    assert_eq!([rd(DATA + 4), rd(DATA + 8)], [0x12345679, 0xaabbccdd]);
    for row in 0..calls {
        // All three backends expose retired PC. Win32's saved context is next-1,
        // meaning the03 byte for CD03, rather than the start of its two bytes.
        assert_eq!([rd(RAW + row * 8), rd(RAW + row * 8 + 4)], [next, next]);
        let actual: Vec<_> = (0..10).map(|slot| rd(ROWS + row * 64 + slot * 4)).collect();
        assert_eq!(
            actual,
            [
                BREAKPOINT,
                0,
                next - 1,
                1,
                0,
                next - 1,
                0x12345678,
                0xaabbccdd,
                rd(DATA + 280),
                rd(DATA + 284)
            ]
        );
    }
    assert_eq!(next - start, if long { 2 } else { 1 });
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
fn cc_and_cd03_use_native_addresses_and_resume_only_from_guest_context_in_veh_and_seh() {
    for long in [false, true] {
        for vectored in [true, false] {
            handled(UnicornCpu::new(PARK_ADDR).unwrap(), long, vectored);
            handled(JitCpu::new(PARK_ADDR, false).unwrap(), long, vectored);
            handled(JitCpu::new(PARK_ADDR, true).unwrap(), long, vectored);
        }
    }
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
fn refused<B: TestCpu>(cpu: B, unknown: bool) {
    let mut machine = machine(cpu);
    let log = Rc::new(RefCell::new(Vec::new()));
    machine.sys.trace = adv_machine::trace::Trace::to_writer(Box::new(Log(log.clone())));
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(if unknown {
        &[0xcd, 0x07][..]
    } else {
        &[0xcc][..]
    });
    let next = code.here();
    store(&mut code, &[0xc7, 0x05], DATA, 1);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.regions(code.bytes.len(), 0);
    machine
        .spawn("unhandled/unknown breakpoint boundary", CODE, &[], 65536)
        .unwrap();
    let RunResult::Failed(error) = machine.run_virtual(1_000_000) else {
        panic!("exception must not be discarded");
    };
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    if unknown {
        assert!(
            error
                .message
                .contains("unmapped x86 CPU exception vector 7")
        );
        assert!(
            error
                .message
                .contains(&format!("original CPU PC {next:#010x}"))
        );
        assert!(
            error
                .message
                .contains(&format!("captured EIP {next:#010x}"))
        );
        let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.warnings.len(), 1);
        assert_eq!(diagnostics.stub_calls, 0);
        let text = String::from_utf8(log.borrow().clone()).unwrap();
        assert!(
            text.contains("# warning") && text.contains(&format!("original CPU PC {next:#010x}"))
        );
    } else {
        assert!(
            error
                .message
                .contains("unhandled Windows exception 0x80000003 at 0x00400000"),
            "{}",
            error.message
        );
    }
}

#[test]
fn unhandled_breakpoints_and_unknown_traps_fail_and_preserve_original_pc_diagnostics() {
    for unknown in [false, true] {
        refused(UnicornCpu::new(PARK_ADDR).unwrap(), unknown);
        refused(JitCpu::new(PARK_ADDR, false).unwrap(), unknown);
        refused(JitCpu::new(PARK_ADDR, true).unwrap(), unknown);
    }
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut state = machine.cpu.capture_architecture().unwrap();
    state.eip = CODE + 2;
    let mismatch = map(
        &machine.cpu,
        &Exception::Trap {
            vector: 0,
            pc: CODE,
        },
        &state,
    )
    .err()
    .unwrap();
    assert!(
        mismatch.contains("PC disagrees")
            && mismatch.contains("original CPU PC 0x00400000")
            && mismatch.contains("captured EIP 0x00400002")
    );
    machine.cpu.write(CODE, &[0x90, 0x90]).unwrap();
    let fabricated = map(
        &machine.cpu,
        &Exception::Trap {
            vector: 3,
            pc: CODE + 2,
        },
        &state,
    )
    .err()
    .unwrap();
    assert!(fabricated.contains("requires an executed CC or CD03"));
    // The normalization hook cannot change even one field for other faults.
    machine.cpu.write(CODE, &[0xf7, 0xf1]).unwrap();
    state.eip = CODE;
    state.regs[1] = 0;
    let record = map(
        &machine.cpu,
        &Exception::Trap {
            vector: 0,
            pc: CODE,
        },
        &state,
    )
    .unwrap();
    let original = state.clone();
    normalize_context(&record, &mut state);
    assert_eq!(state, original);
}
