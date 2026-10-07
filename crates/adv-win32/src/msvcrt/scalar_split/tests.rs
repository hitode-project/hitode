//! VC80/90 oracle: target/engine-loop/20261007-resume/crt-split-native/probe.c,
//! SHA256 11524B6683F573AE2047EBCBF181E021D83E8CB74FAA2C8DD0E195A33D5BA084.
//! Normal values are exact at PC24/53/64 and all four rounding directions.
use super::*;
use adv_abi::{Conv, signature::AbiKind};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const LOWER: u32 = DATA + 7000;
const CONTROL: u32 = LOWER + 16;
const ERRNO: u32 = CONTROL + 8;
const ESP: u32 = ERRNO + 4;
const ALIAS: u32 = ESP + 4;
const CASES: [(u64, u64, u64); 10] = [
    (0, 0, 0),
    (SIGN, SIGN, SIGN),
    (0x4004000000000000, 0x4000000000000000, 0x3fe0000000000000),
    (0xc004000000000000, 0xc000000000000000, 0xbfe0000000000000),
    (0x3fe8000000000000, 0, 0x3fe8000000000000),
    (0xbfe8000000000000, SIGN, 0xbfe8000000000000),
    (0x4340000000000000, 0x4340000000000000, 0),
    (0x7ff0000000000000, 0x7ff0000000000000, 0),
    (0xfff0000000000000, 0xfff0000000000000, SIGN),
    (0x3ff123456789abcd, 0x3ff0000000000000, 0x3fb23456789abcd0),
];

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        let signature = adv_abi::signature::lookup(module, "modf").unwrap();
        assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 3 }));
        assert_eq!(signature.return_kind, AbiKind::F64);
        assert_eq!(signature.parameters[0].kind, AbiKind::F64);
        assert_eq!(signature.parameters[0].words, 2);
        assert_eq!(signature.parameters[1].kind, AbiKind::Pointer);
        assert!(registry.dynamic_available(module, "modf"));
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-split-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
    machine.cpu.write(DATA, &[0xcc; 7000]).unwrap();
    machine.cpu.write(LOWER, &3.25f64.to_le_bytes()).unwrap();
    machine
        .cpu
        .write(LOWER + 8, &(-7.5f64).to_le_bytes())
        .unwrap();
    machine
}

fn memory(code: &mut Asm, opcode: &[u8], address: u32) {
    code.bytes.extend_from_slice(opcode);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}
fn lower(code: &mut Asm) {
    memory(code, &[0xdd, 0x05], LOWER);
    memory(code, &[0xdd, 0x05], LOWER + 8);
}
fn bits<B: Backend>(machine: &Machine<B>, address: u32) -> u64 {
    let mut bytes = [0; 8];
    machine.cpu.read(address, &mut bytes).unwrap();
    u64::from_le_bytes(bytes)
}
fn seed<B: Backend>(machine: &mut Machine<B>, code: &mut Asm, module: &str) {
    let errno = machine.resolve_import(module, "_errno").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    code.call(errno).mov_mem_eax(ERRNO);
    code.bytes.extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
    code.bytes.extend_from_slice(&[0xc7, 0x40, 4]);
    code.bytes.extend_from_slice(&91u32.to_le_bytes());
    code.push_imm(77).call(set_error);
}
fn argument(code: &mut Asm, entry: u32, input: u64) {
    // Output pointer is already pushed; double consumes exactly two words.
    code.push_imm((input >> 32) as u32).push_imm(input as u32);
    code.bytes.push(0xb8);
    code.bytes.extend_from_slice(&0xaabbccddu32.to_le_bytes());
    code.bytes.push(0xba);
    code.bytes.extend_from_slice(&0x11223344u32.to_le_bytes());
    code.call(entry).add_esp(12);
}
fn execute<B: Backend>(machine: &mut Machine<B>, code: Asm) -> RunResult {
    assert!(code.bytes.len() < 16384);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("typed cdecl binary64 split", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}

fn guest<B: Backend>(cpu: impl Fn() -> B) {
    let mut machine = fixture(cpu());
    let before = machine.cpu.fpu_environment().unwrap();
    let controls = [0x007fu16, 0x027f, 0x0f7f];
    for (index, control) in controls.into_iter().enumerate() {
        machine
            .cpu
            .write(CONTROL + index as u32 * 2, &control.to_le_bytes())
            .unwrap();
    }
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(ESP);
    let mut outputs = Vec::new();
    for (module_index, module) in MODULES.into_iter().enumerate() {
        memory(&mut code, &[0xd9, 0x2d], CONTROL + module_index as u32 * 2);
        let entry = machine.resolve_import(module, "modf").unwrap();
        for (input, whole, fraction) in CASES {
            let output = DATA + outputs.len() as u32 * 80;
            seed(&mut machine, &mut code, module);
            lower(&mut code);
            code.push_imm(output + 8);
            argument(&mut code, entry, input);
            code.mov_mem_eax(output + 40);
            memory(&mut code, &[0x89, 0x15], output + 44);
            for offset in [16, 24, 32] {
                memory(&mut code, &[0xdd, 0x1d], output + offset);
            }
            code.bytes.extend_from_slice(&[0x89, 0xe0]);
            code.mov_mem_eax(output + 48);
            code.mov_eax_mem(ERRNO)
                .mov_eax_ptr_eax()
                .mov_mem_eax(output + 52);
            code.mov_eax_mem(ERRNO)
                .bytes
                .extend_from_slice(&[0x8b, 0x40, 4]);
            code.mov_mem_eax(output + 56);
            let get_error = machine
                .resolve_import("kernel32.dll", "GetLastError")
                .unwrap();
            code.call(get_error).mov_mem_eax(output + 60);
            memory(&mut code, &[0xd9, 0x3d], output + 64);
            outputs.push((output, whole, fraction, controls[module_index]));
        }
    }
    // Output overlaps both input argument DWORDs, after their typed snapshot.
    let entry = machine.resolve_import("msvcr90.dll", "modf").unwrap();
    code.bytes.extend_from_slice(&[0x89, 0xe0, 0x83, 0xe8, 12]);
    code.mov_mem_eax(ALIAS).bytes.push(0x50);
    argument(&mut code, entry, (-2.5f64).to_bits());
    memory(&mut code, &[0xdd, 0x1d], ALIAS + 8);
    code.mov_eax_mem(ALIAS)
        .mov_eax_ptr_eax()
        .mov_mem_eax(ALIAS + 16);
    code.mov_eax_mem(ALIAS)
        .bytes
        .extend_from_slice(&[0x8b, 0x40, 4]);
    code.mov_mem_eax(ALIAS + 20);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    let result = execute(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (output, whole, fraction, control) in outputs {
        assert_eq!(bits(&machine, output), 0xcccccccccccccccc);
        assert_eq!(bits(&machine, output + 8), whole);
        assert_eq!(bits(&machine, output + 16), fraction);
        assert_eq!(bits(&machine, output + 24), (-7.5f64).to_bits());
        assert_eq!(bits(&machine, output + 32), 3.25f64.to_bits());
        for (offset, expected) in [
            (40, 0xaabbccdd),
            (44, 0x11223344),
            (48, machine.cpu.read_u32(ESP).unwrap()),
            (52, 79),
            (56, 91),
            (60, 77),
        ] {
            assert_eq!(machine.cpu.read_u32(output + offset).unwrap(), expected);
        }
        let mut word = [0; 2];
        machine.cpu.read(output + 64, &mut word).unwrap();
        assert_eq!(u16::from_le_bytes(word), control);
        assert_eq!(machine.cpu.read_u32(output + 68).unwrap(), 0xcccccccc);
    }
    assert_eq!(bits(&machine, ALIAS + 16), (-2.0f64).to_bits());
    assert_eq!(bits(&machine, ALIAS + 8), (-0.5f64).to_bits());
    let after = machine.cpu.fpu_environment().unwrap();
    assert_eq!(after.x87_control, controls[2]);
    assert_eq!(after.mxcsr, before.mxcsr);

    // Read guards and unsupported FP status fail before publishing anything.
    for (input, output, protected_input) in [
        (2.5f64.to_bits(), 0, false),
        (2.5f64.to_bits(), DATA + 8188, false),
        (2.5f64.to_bits(), DATA + 8, true),
        (0x7ff8000000000042, DATA + 8, false),
        (0x7ff0000000000042, DATA + 8, false),
        (1, DATA + 8, false),
    ] {
        let mut machine = fixture(cpu());
        let entry = machine.resolve_import("msvcr80.dll", "modf").unwrap();
        let mut code = Asm::new(CODE);
        seed(&mut machine, &mut code, "msvcr80.dll");
        lower(&mut code);
        if protected_input {
            machine.cpu.map(DATA + 8192, 4096, Prot::WRITE).unwrap();
            for (address, word) in [
                (DATA + 8184, PARK_ADDR),
                (DATA + 8188, input as u32),
                (DATA + 8192, (input >> 32) as u32),
                (DATA + 8196, output),
            ] {
                machine.cpu.write(address, &word.to_le_bytes()).unwrap();
            }
            code.bytes.push(0xbc);
            code.bytes.extend_from_slice(&(DATA + 8184).to_le_bytes());
            code.jmp(entry);
        } else {
            if output == DATA + 8188 {
                machine.cpu.map(DATA + 8192, 4096, Prot::READ).unwrap();
                machine.cpu.write_u32(output, 0xcccccccc).unwrap();
            }
            code.push_imm(output);
            argument(&mut code, entry, input);
            code.ret();
        }
        let result = execute(&mut machine, code);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert_eq!(
            machine.cpu.pop_f64().unwrap().to_bits(),
            (-7.5f64).to_bits()
        );
        assert_eq!(machine.cpu.pop_f64().unwrap().to_bits(), 3.25f64.to_bits());
        if output != 0 {
            assert_eq!(machine.cpu.read_u32(output).unwrap(), 0xcccccccc);
        }
        let errno = machine.cpu.read_u32(ERRNO).unwrap();
        assert_eq!(machine.cpu.read_u32(errno).unwrap(), 79);
        assert_eq!(machine.cpu.read_u32(errno + 4).unwrap(), 91);
        if matches!(input, 0x7ff8000000000042 | 0x7ff0000000000042 | 1) {
            let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
            assert_eq!(diagnostics.warnings.len(), 1);
            assert!(
                diagnostics
                    .warnings
                    .keys()
                    .any(|(api, _)| api == "msvcr80.dll!modf")
            );
        }
    }

    // A write-only destination needs no artificial read permission.
    let mut machine = fixture(cpu());
    let entry = machine.resolve_import("msvcr90.dll", "modf").unwrap();
    machine.cpu.map(DATA + 8192, 4096, Prot::WRITE).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(DATA + 8192);
    argument(&mut code, entry, (-2.5f64).to_bits());
    memory(&mut code, &[0xdd, 0x1d], DATA + 16);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    let result = execute(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    machine.cpu.protect(DATA + 8192, 4096, Prot::RW).unwrap();
    assert_eq!(bits(&machine, DATA + 8192), (-2.0f64).to_bits());
    assert_eq!(bits(&machine, DATA + 16), (-0.5f64).to_bits());
}

#[test]
fn typed_modf_preserves_parts_x87_cdecl_aliases_and_atomic_memory_guards() {
    guest(|| UnicornCpu::new(PARK_ADDR).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, false).unwrap());
}
