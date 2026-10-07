//! Native normal scaling oracle: crt-ldexp-native/probe.c, SHA256
//! 3F268318B2A50A9E982427527263BF2FE467678E64834C8618C6280CA15F0BAF.
use super::*;
use adv_abi::{Conv, signature::AbiKind};
use adv_cpu::{
    JitCpu, X87Register,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const LOWER: u32 = DATA + 4096;
const CONTROL: u32 = LOWER + 32;
const ERRNO: u32 = CONTROL + 8;
const ESP: u32 = ERRNO + 4;
const SAVED: u32 = DATA + 8192;
const EXTENDED: X87Register = X87Register([1, 0, 0, 0, 0, 0, 0, 0x80, 0xff, 0x3f]);
const GOLDEN: [(u64, i32, u64); 5] = [
    (0xc130000000000000, -20, 0xbff0000000000000),
    (0, -788, 0),
    (0x8000000000000000, 235, 0x8000000000000000),
    (0x3ff0000000000000, -788, 0x0eb0000000000000),
    (0x413fffff00000000, 235, 0x4fefffff00000000),
];

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        let abi = adv_abi::signature::lookup(module, "ldexp").unwrap();
        assert_eq!(abi.conv, Some(Conv::Cdecl { argc: 3 }));
        assert_eq!(abi.return_kind, AbiKind::F64);
        assert_eq!(abi.parameters[0].words, 2);
        assert_eq!(abi.parameters[1].words, 1);
        assert_eq!(
            registry.get(module, "ldexp").unwrap().conv,
            Conv::Cdecl { argc: 3 }
        );
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ldexp-tests");
    crate::Win32::install(&mut m, crate::Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 16384, Prot::RW).unwrap();
    m.cpu.write(LOWER, &EXTENDED.0).unwrap();
    m.cpu.write(LOWER + 16, &(-7.5f64).to_le_bytes()).unwrap();
    m
}
fn absolute(code: &mut Asm, opcode: &[u8], address: u32) {
    code.bytes.extend_from_slice(opcode);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}
fn seed<B: Backend>(m: &mut Machine<B>, code: &mut Asm, module: &str, control: u16) {
    m.cpu.write(CONTROL, &control.to_le_bytes()).unwrap();
    let errno = m.resolve_import(module, "_errno").unwrap();
    code.call(errno).mov_mem_eax(ERRNO);
    code.bytes.extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
    code.bytes.extend_from_slice(&[0xc7, 0x40, 4]);
    code.bytes.extend_from_slice(&91u32.to_le_bytes());
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    code.push_imm(77).call(set_error);
    absolute(code, &[0xd9, 0x2d], CONTROL);
    code.bytes.extend_from_slice(&[0xdb, 0xe2]); // FNCLEX.
    absolute(code, &[0xdb, 0x2d], LOWER);
    absolute(code, &[0xdd, 0x05], LOWER + 16);
}
fn save(code: &mut Asm) {
    absolute(code, &[0x0f, 0xae, 0x05], SAVED);
    absolute(code, &[0x66, 0x81, 0x0d], SAVED + 2);
    code.bytes.extend_from_slice(&0x0200u16.to_le_bytes());
    absolute(code, &[0x0f, 0xae, 0x0d], SAVED);
}
fn arguments(code: &mut Asm, input: u64, power: i32) {
    code.push_imm(power as u32)
        .push_imm((input >> 32) as u32)
        .push_imm(input as u32);
}
fn bits<B: Backend>(m: &Machine<B>, address: u32) -> u64 {
    let mut bytes = [0; 8];
    m.cpu.read(address, &mut bytes).unwrap();
    u64::from_le_bytes(bytes)
}
fn execute<B: Backend>(m: &mut Machine<B>, code: Asm) -> RunResult {
    assert!(code.bytes.len() < 16384);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("scalar binary64 scaling", CODE, &[], 65536)
        .unwrap();
    m.run_virtual(1_000_000)
}
fn contract<B: Backend>(cpu: impl Fn() -> B) {
    let mut m = machine(cpu());
    let mut code = Asm::new(CODE);
    let mut outputs = Vec::new();
    let mut errno_cells = Vec::new();
    for (module_index, module) in MODULES.into_iter().enumerate() {
        let entry = m.resolve_import(module, "ldexp").unwrap();
        for (case, &(input, power, expected)) in GOLDEN.iter().enumerate() {
            let out = DATA + outputs.len() as u32 * 80;
            let control = if case == 2 || case == 3 {
                0x037f
            } else {
                0x027f
            };
            // A separate per-case CW location avoids later cases overwriting
            // an earlier FLDCW operand before the actual guest program runs.
            seed(&mut m, &mut code, module, control);
            let cw = DATA + 12288 + outputs.len() as u32 * 2;
            m.cpu.write(cw, &control.to_le_bytes()).unwrap();
            absolute(&mut code, &[0xd9, 0x2d], cw);
            save(&mut code);
            absolute(&mut code, &[0xdd, 0x3d], out + 72);
            code.bytes.extend_from_slice(&[0x89, 0xe0]);
            code.mov_mem_eax(ESP);
            arguments(&mut code, input, power);
            code.call(entry).add_esp(12);
            absolute(&mut code, &[0xdd, 0x3d], out + 58);
            absolute(&mut code, &[0xdd, 0x1d], out);
            absolute(&mut code, &[0xdd, 0x1d], out + 8);
            absolute(&mut code, &[0xdb, 0x3d], out + 16);
            code.bytes.extend_from_slice(&[0x89, 0xe0]);
            code.mov_mem_eax(out + 32);
            code.mov_eax_mem(ERRNO).mov_mem_eax(out + 60);
            code.mov_eax_ptr_eax().mov_mem_eax(out + 36);
            code.mov_eax_mem(ERRNO)
                .bytes
                .extend_from_slice(&[0x8b, 0x40, 4]);
            code.mov_mem_eax(out + 40);
            let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
            code.call(get_error).mov_mem_eax(out + 44);
            absolute(&mut code, &[0xd9, 0x3d], out + 56);
            outputs.push((out, input, expected, control, module_index));
        }
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    assert!(matches!(execute(&mut m, code), RunResult::Exited(0)));
    for (out, input, expected, control, module_index) in outputs {
        assert_eq!(bits(&m, out), expected);
        assert_eq!(bits(&m, out + 8), (-7.5f64).to_bits());
        let mut lower = [0; 10];
        m.cpu.read(out + 16, &mut lower).unwrap();
        assert_eq!(lower, EXTENDED.0);
        assert_eq!(
            m.cpu.read_u32(out + 32).unwrap(),
            m.cpu.read_u32(ESP).unwrap()
        );
        for (offset, expected) in [(36, 79), (40, 91), (44, 77)] {
            assert_eq!(m.cpu.read_u32(out + offset).unwrap(), expected);
        }
        assert_eq!(m.cpu.read_u16(out + 56).unwrap(), control);
        let condition = if module_index == 0 || input >> 63 != 0 {
            0
        } else if input == 0 {
            0x4000
        } else {
            0x0100
        };
        let condition = if module_index != 0 && input & !(1 << 63) == 0 {
            0x4000
        } else {
            condition
        };
        assert_eq!(m.cpu.read_u16(out + 58).unwrap(), 0x2800 | condition);
        errno_cells.push((module_index, m.cpu.read_u32(out + 60).unwrap()));
    }
    assert_ne!(errno_cells[0].1, errno_cells[5].1);
    assert_ne!(errno_cells[5].1, errno_cells[10].1);

    for (input, power, control, full) in [
        (f64::INFINITY.to_bits(), 0, 0x027f, false),
        (1, 1, 0x027f, false),
        (1f64.to_bits(), 1024, 0x027f, false),
        (1f64.to_bits(), -1075, 0x027f, false),
        (1f64.to_bits(), 1, 0x067f, false),
        (1f64.to_bits(), 1, 0x027e, false),
        (1f64.to_bits(), 1, 0x027f, true),
    ] {
        let mut m = machine(cpu());
        let mut code = Asm::new(CODE);
        seed(&mut m, &mut code, "msvcr90.dll", control);
        if full {
            for _ in 0..6 {
                code.bytes.extend_from_slice(&[0xd9, 0xe8]);
            }
        }
        save(&mut code);
        let entry = m.resolve_import("msvcr90.dll", "ldexp").unwrap();
        arguments(&mut code, input, power);
        code.call(entry).ret();
        assert!(matches!(execute(&mut m, code), RunResult::Failed(_)));
        let state = m.cpu.capture_architecture().unwrap();
        assert_eq!(state.x87.control, m.cpu.read_u16(SAVED).unwrap());
        assert_eq!(state.x87.status, m.cpu.read_u16(SAVED + 2).unwrap());
        for logical in 0..if full { 8 } else { 2 } {
            let mut raw = [0; 10];
            m.cpu
                .read(SAVED + 32 + logical as u32 * 16, &mut raw)
                .unwrap();
            assert_eq!(state.x87.regs[state.x87.physical_index(logical)].0, raw);
        }
        let errno = m.cpu.read_u32(ERRNO).unwrap();
        assert_eq!(m.cpu.read_u32(errno).unwrap(), 79);
        assert_eq!(m.cpu.read_u32(errno + 4).unwrap(), 91);
        assert_eq!(
            m.cpu
                .read_u32(m.sys.contexts[0].teb + adv_machine::layout::teb::LAST_ERROR)
                .unwrap(),
            77
        );
        assert!(
            m.ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .keys()
                .any(|(api, _)| api == "msvcr90.dll!ldexp")
        );
    }
}

#[test]
fn ldexp_typed_stack_scaling_preserves_binary64_sign_controls_and_error_owners() {
    contract(|| JitCpu::new(PARK_ADDR, false).unwrap());
    contract(|| JitCpu::new(PARK_ADDR, true).unwrap());
    contract(|| UnicornCpu::new(PARK_ADDR).unwrap());
}
