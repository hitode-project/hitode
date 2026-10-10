use super::*;
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const HANDLER: u32 = CODE + 512;
const VALUE: u32 = DATA + 256;
const DONE: u32 = DATA + 80;

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
        .join("../../target/arithmetic-exception-tests");
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

#[derive(Clone, Copy)]
enum Case {
    Zero,
    UnsignedOverflow,
    SignedOverflow,
    Illegal,
    Cli,
    Sti,
}
fn exercise<B: TestCpu>(cpu: B, case: Case, vectored: bool) {
    let mut machine = machine(cpu);
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
    let (low, high, divisor, expected_code, result, remainder) = match case {
        Case::Zero => (42, 0, 0, INTEGER_DIVIDE_BY_ZERO, 14, 0),
        Case::UnsignedOverflow => (5, 3, 3, INTEGER_OVERFLOW, 1, 2),
        Case::SignedOverflow => (
            0x80000000,
            u32::MAX,
            u32::MAX,
            INTEGER_OVERFLOW,
            0x80000000,
            0,
        ),
        Case::Illegal | Case::Cli | Case::Sti => (
            0x12345678,
            0x23456789,
            0x34567890,
            if matches!(case, Case::Illegal) {
                ILLEGAL_INSTRUCTION
            } else {
                PRIVILEGED_INSTRUCTION
            },
            0x12345678,
            0x23456789,
        ),
    };
    imm(&mut code, &[0xb8], low);
    imm(&mut code, &[0xba], high);
    imm(&mut code, &[0xb9], divisor);
    imm(&mut code, &[0xbb], 0xaabbccdd);
    imm(&mut code, &[0x89, 0x25], DATA + 84); // interrupted ESP, above dispatcher scratch.
    code.bytes.extend_from_slice(&[0x9c, 0x8f, 0x05]);
    code.bytes.extend_from_slice(&(DATA + 88).to_le_bytes()); // original flags.
    let fault = code.here();
    code.bytes.extend_from_slice(match case {
        Case::Zero | Case::UnsignedOverflow => &[0xf7, 0xf1], // div ecx
        Case::SignedOverflow => &[0xf7, 0xf9],                // idiv ecx
        Case::Illegal => &[0x0f, 0x0b],                       // ud2
        Case::Cli => &[0xfa],
        Case::Sti => &[0xfb],
    });
    let next = code.here();
    code.mov_mem_eax(DATA + 64);
    imm(&mut code, &[0x89, 0x15], DATA + 68); // resumed EDX.
    imm(&mut code, &[0x89, 0x1d], DATA + 72); // resumed EBX.
    store(&mut code, &[0xc7, 0x05], DONE, 1);
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
    } // ECX=EXCEPTION_RECORD, EDX=CONTEXT.
    for (slot, offset) in [0_u8, 4, 12, 16].into_iter().enumerate() {
        handler.bytes.extend_from_slice(&[0x8b, 0x41, offset]);
        handler.mov_mem_eax(DATA + slot as u32 * 4);
    }
    for (slot, offset) in [0xb8_u32, 0xb0, 0xa8, 0xac, 0xa4, 0xc4, 0xc0]
        .into_iter()
        .enumerate()
    {
        imm(&mut handler, &[0x8b, 0x82], offset);
        handler.mov_mem_eax(DATA + 16 + slot as u32 * 4);
    }
    imm(&mut handler, &[0xff, 0x05], DATA + 48); // callback count.
    // Repair the saved state, then retry DIV at its original EIP. The handler
    // explicitly skips illegal/privileged instructions by writing CONTEXT.Eip.
    let (offset, value) = match case {
        Case::Zero => (0xac, 3),
        Case::UnsignedOverflow => (0xa8, 0),
        Case::SignedOverflow => (0xac, 1),
        Case::Illegal | Case::Cli | Case::Sti => (0xb8, next),
    };
    store(&mut handler, &[0xc7, 0x82], offset, value);
    imm(&mut handler, &[0xbb], 0xdeadbeef); // live callback clobber must not escape.
    imm(&mut handler, &[0xb8], if vectored { u32::MAX } else { 0 });
    handler.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.write(HANDLER, &handler.bytes).unwrap();
    machine.cpu.regions(code.bytes.len(), handler.bytes.len());
    machine
        .spawn("precise Windows arithmetic exception", CODE, &[], 65536)
        .unwrap();
    let outcome = machine.run_virtual(1_000_000);
    assert!(matches!(outcome, RunResult::Exited(0)), "{outcome:?}");
    let rd = |slot| machine.cpu.read_u32(DATA + slot).unwrap();
    assert_eq!([rd(0), rd(4), rd(8), rd(12)], [expected_code, 0, fault, 0]);
    assert_eq!(
        [rd(16), rd(20), rd(24), rd(28), rd(32)],
        [fault, low, high, divisor, 0xaabbccdd]
    );
    assert_eq!(rd(36), rd(84));
    assert_eq!(rd(40), rd(88) | (1 << 16)); // Native fault CONTEXT saves RF.
    assert_eq!(rd(48), 1);
    assert_eq!(
        [rd(64), rd(68), rd(72), rd(80)],
        [result, remainder, 0xaabbccdd, 1]
    );
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
    let env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    assert_eq!(super::super::phase::uncaught_query_blocker(&env), None);
}

#[test]
fn user_interrupt_flag_instructions_preserve_state_and_resume_after_guest_context_repair() {
    for case in [Case::Cli, Case::Sti] {
        for vectored in [true, false] {
            // Unicorn's flat mode does not model CPL3 privilege checks.
            exercise(JitCpu::new(PARK_ADDR, false).unwrap(), case, vectored);
            exercise(JitCpu::new(PARK_ADDR, true).unwrap(), case, vectored);
        }
    }
}

#[test]
fn general_protection_mapping_requires_a_privileged_opcode_and_handles_page_end() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut state = machine.cpu.capture_architecture().unwrap();
    state.eip = CODE + 4095;
    machine.cpu.write(state.eip, &[0xfb]).unwrap();
    let event = Exception::Trap {
        pc: state.eip,
        vector: 13,
    };
    assert_eq!(
        map(&machine.cpu, &event, &state).unwrap().code,
        PRIVILEGED_INSTRUCTION
    );
    machine.cpu.write(state.eip, &[0x90]).unwrap();
    assert!(
        map(&machine.cpu, &event, &state)
            .err()
            .expect("unrelated general protection must remain unclassified")
            .contains("unmapped x86 CPU exception vector 13")
    );
}

#[test]
fn divide_zero_overflow_and_illegal_instruction_resume_only_after_guest_context_repair() {
    for case in [
        Case::Zero,
        Case::UnsignedOverflow,
        Case::SignedOverflow,
        Case::Illegal,
    ] {
        for vectored in [true, false] {
            exercise(UnicornCpu::new(PARK_ADDR).unwrap(), case, vectored);
            exercise(JitCpu::new(PARK_ADDR, false).unwrap(), case, vectored);
            exercise(JitCpu::new(PARK_ADDR, true).unwrap(), case, vectored);
        }
    }
}

#[test]
fn divide_classification_decodes_register_width_aliases_and_memory_addressing() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut state = machine.cpu.capture_architecture().unwrap();
    state.eip = CODE;
    state.regs[1] = 0x1234_0000;
    // CX=0, CL=0, CH=0, but ECX!=0: classify the actual operand width.
    for (bytes, expected) in [
        (&[0x66, 0xf7, 0xf1][..], INTEGER_DIVIDE_BY_ZERO),
        (&[0xf6, 0xf1][..], INTEGER_DIVIDE_BY_ZERO),
        (&[0xf6, 0xf5][..], INTEGER_DIVIDE_BY_ZERO),
        (&[0xf7, 0xf1][..], INTEGER_OVERFLOW),
    ] {
        machine.cpu.write(CODE, bytes).unwrap();
        let record = map(
            &machine.cpu,
            &Exception::Trap {
                vector: 0,
                pc: CODE,
            },
            &state,
        )
        .unwrap();
        assert_eq!(record.code, expected);
    }
    state.regs[0] = 0x0012_0000; // actual Wing DIV EAX=0 is tested below too.
    for (low, expected) in [(0, INTEGER_DIVIDE_BY_ZERO), (3, INTEGER_OVERFLOW)] {
        state.regs[0] = low;
        machine.cpu.write(CODE, &[0xf7, 0xf0]).unwrap();
        assert_eq!(
            map(
                &machine.cpu,
                &Exception::Trap {
                    vector: 0,
                    pc: CODE
                },
                &state
            )
            .unwrap()
            .code,
            expected
        );
    }
    // DS:[ebx+ecx*4+8], 16-bit DS:[bx+si], and FS:[eax+0x20].
    let memories: [(&[u8], u32, u32, [u32; 8]); 3] = [
        (
            &[0xf7, 0x74, 0x8b, 0x08],
            VALUE,
            0,
            [0, 3, 0, VALUE - 20, 0, 0, 0, 0],
        ),
        (
            &[0x67, 0x66, 0xf7, 0x30],
            0x1080,
            0,
            [0, 0, 0, 0x1000, 0, 0, 0x80, 0],
        ),
        (
            &[0x64, 0xf7, 0x70, 0x20],
            VALUE,
            DATA,
            [VALUE - DATA - 0x20, 0, 0, 0, 0, 0, 0, 0],
        ),
    ];
    machine.cpu.map(0x1000, 4096, Prot::RW).unwrap();
    for (bytes, address, fs, regs) in memories {
        state.regs = regs;
        state.fs_base = fs;
        machine.cpu.write(CODE, bytes).unwrap();
        for (value, expected) in [(0, INTEGER_DIVIDE_BY_ZERO), (1, INTEGER_OVERFLOW)] {
            machine.cpu.write_u32(address, value).unwrap();
            assert_eq!(
                map(
                    &machine.cpu,
                    &Exception::Trap {
                        vector: 0,
                        pc: CODE
                    },
                    &state
                )
                .unwrap()
                .code,
                expected
            );
        }
    }
    // A two-byte DIV at a page's end is fully readable without a mapped next page.
    state.eip = CODE + 4094;
    state.regs[1] = 0;
    machine.cpu.write(state.eip, &[0xf7, 0xf1]).unwrap();
    assert_eq!(
        map(
            &machine.cpu,
            &Exception::Trap {
                vector: 0,
                pc: state.eip
            },
            &state
        )
        .unwrap()
        .code,
        INTEGER_DIVIDE_BY_ZERO
    );
}

fn failure<B: TestCpu>(cpu: B, mode: u8) {
    let mut machine = machine(cpu);
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(if mode == 1 {
        &[0xcd, 0x07][..]
    } else {
        &[0x31, 0xc0, 0xf7, 0xf0][..]
    });
    store(&mut code, &[0xc7, 0x05], DONE, 1);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.regions(code.bytes.len(), 0);
    machine
        .spawn("unsupported or unhandled CPU exception", CODE, &[], 65536)
        .unwrap();
    let RunResult::Failed(failure) = machine.run_virtual(1_000_000) else {
        panic!("must not skip the exception");
    };
    assert!(
        failure.message.contains(match mode {
            0 => "unhandled Windows exception 0xc0000094",
            1 => "unmapped x86 CPU exception vector 7",
            _ => unreachable!("only unhandled and unmapped stops are negative cases"),
        }),
        "{}",
        failure.message
    );
    assert_eq!(machine.cpu.read_u32(DONE).unwrap(), 0);
    if mode == 1 {
        let diagnostic = machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostic.warnings.len(), 1);
        assert_eq!(diagnostic.stub_calls, 0);
    }
}

#[test]
fn unhandled_and_unmapped_exceptions_fail_without_advancing_guest_work() {
    // Legitimate nested frame searches are covered by the native-backed
    // adv-runtime exception_dispatch fixture, including EXCEPTION_NESTED_CALL.
    for mode in 0..2 {
        failure(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
        failure(JitCpu::new(PARK_ADDR, true).unwrap(), mode);
    }
}
