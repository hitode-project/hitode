//! Authored VC80/90 x87 oracle: crt-power-native/probe.c, SHA256
//! 5F2BD9C4688A25E57358CBB129FD2B0BFAD1BC49981469F034F7184857C3B1C4.
//! Ordinary transcendental output is compared within two binary64 ULPs;
//! stack payloads, sticky/condition flags, controls and errors are exact.
use super::*;
use adv_abi::{Conv, signature::AbiKind};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const INPUT: u32 = DATA + 4096;
const LOWER: u32 = INPUT + 32;
const CONTROL: u32 = LOWER + 24;
const ERRNO: u32 = CONTROL + 8;
const ESP: u32 = ERRNO + 4;
const EXTENDED: X87Register = X87Register([1, 0, 0, 0, 0, 0, 0, 0x80, 0xff, 0x3f]);
const GOLDEN: [(f64, f64, u64); 8] = [
    (2.0, 3.0, 0x4020000000000000),
    (3.0, 2.0, 0x4022000000000000),
    (2.0, 0.5, 0x3ff6a09e667f3bcd),
    (16.0, 0.5, 0x4010000000000000),
    (10.0, -3.0, 0x3f50624dd2f1a9fc),
    (1.0, 0.5, 0x3ff0000000000000),
    (0.0, 3.0, 0),
    (-2.0, 3.0, 0xc020000000000000),
];

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        let signature = adv_abi::signature::lookup(module, "_CIpow").unwrap();
        assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 0 }));
        assert_eq!(signature.return_kind, AbiKind::Void);
        assert!(signature.parameters.is_empty());
        assert!(registry.dynamic_available(module, "_CIpow"));
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-power-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
    machine.cpu.write(LOWER, &EXTENDED.0).unwrap();
    machine
        .cpu
        .write(LOWER + 16, &(-7.5f64).to_le_bytes())
        .unwrap();
    machine
}
fn absolute(code: &mut Asm, opcode: &[u8], address: u32) {
    code.bytes.extend_from_slice(opcode);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}
fn lower(code: &mut Asm) {
    absolute(code, &[0xdb, 0x2d], LOWER); // fld tbyte: retain all 64 significand bits.
    absolute(code, &[0xdd, 0x05], LOWER + 16);
}
fn bits<B: Backend>(machine: &Machine<B>, address: u32) -> u64 {
    let mut bytes = [0; 8];
    machine.cpu.read(address, &mut bytes).unwrap();
    u64::from_le_bytes(bytes)
}
fn word<B: Backend>(machine: &Machine<B>, address: u32) -> u16 {
    let mut bytes = [0; 2];
    machine.cpu.read(address, &mut bytes).unwrap();
    u16::from_le_bytes(bytes)
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
fn execute<B: Backend>(machine: &mut Machine<B>, code: Asm) -> RunResult {
    assert!(code.bytes.len() < 16384);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("zero-word x87 intrinsic power", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}

fn guest<B: Backend>(cpu: impl Fn() -> B) {
    let mut machine = fixture(cpu());
    let before = machine.cpu.fpu_environment().unwrap();
    machine
        .cpu
        .write(CONTROL, &0x027fu16.to_le_bytes())
        .unwrap();
    machine
        .cpu
        .write(CONTROL + 2, &0x037fu16.to_le_bytes())
        .unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(ESP);
    // Pre-existing ZE and C0 survive native _CIpow, including an exact result.
    code.bytes.extend_from_slice(&[
        0xdb, 0xe2, // fnclex
        0xd9, 0xe8, 0xd9, 0xee, 0xde, 0xf9, 0xdd, 0xd8, // 1/0, discard.
        0xd9, 0xe8, 0xd9, 0xee, 0xde, 0xd9, // 0<1, fcompp.
    ]);
    let mut outputs = Vec::new();
    for (module_index, module) in MODULES.into_iter().enumerate() {
        let control = if module_index == 1 { 0x037f } else { 0x027f };
        absolute(
            &mut code,
            &[0xd9, 0x2d],
            CONTROL + u32::from(module_index == 1) * 2,
        );
        let entry = machine.resolve_import(module, "_CIpow").unwrap();
        for (base, exponent, expected) in GOLDEN {
            let index = outputs.len() as u32;
            let input = INPUT + 256 + index * 16;
            let output = DATA + index * 80;
            machine.cpu.write(input, &base.to_le_bytes()).unwrap();
            machine
                .cpu
                .write(input + 8, &exponent.to_le_bytes())
                .unwrap();
            seed(&mut machine, &mut code, module);
            lower(&mut code);
            absolute(&mut code, &[0xdd, 0x05], input);
            absolute(&mut code, &[0xdd, 0x05], input + 8);
            code.bytes.push(0xb8);
            code.bytes.extend_from_slice(&0xaabbccddu32.to_le_bytes());
            code.bytes.push(0xba);
            code.bytes.extend_from_slice(&0x11223344u32.to_le_bytes());
            code.call(entry); // No integer-stack arguments or caller cleanup.
            code.mov_mem_eax(output + 32);
            absolute(&mut code, &[0x89, 0x15], output + 36);
            absolute(&mut code, &[0xdd, 0x3d], output + 58); // fnstsw before popping.
            absolute(&mut code, &[0xdd, 0x1d], output);
            absolute(&mut code, &[0xdd, 0x1d], output + 8);
            absolute(&mut code, &[0xdb, 0x3d], output + 16); // fstp tbyte.
            code.bytes.extend_from_slice(&[0x89, 0xe0]);
            code.mov_mem_eax(output + 40);
            code.mov_eax_mem(ERRNO)
                .mov_eax_ptr_eax()
                .mov_mem_eax(output + 44);
            code.mov_eax_mem(ERRNO)
                .bytes
                .extend_from_slice(&[0x8b, 0x40, 4]);
            code.mov_mem_eax(output + 48);
            let get_error = machine
                .resolve_import("kernel32.dll", "GetLastError")
                .unwrap();
            code.call(get_error).mov_mem_eax(output + 52);
            absolute(&mut code, &[0xd9, 0x3d], output + 56);
            outputs.push((output, expected, control));
        }
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    let result = execute(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (output, expected, control) in outputs {
        assert!(bits(&machine, output).abs_diff(expected) <= 2);
        assert_eq!(bits(&machine, output + 8), (-7.5f64).to_bits());
        let mut extended = [0; 10];
        machine.cpu.read(output + 16, &mut extended).unwrap();
        assert_eq!(extended, EXTENDED.0);
        for (offset, expected) in [
            (32, 0xaabbccdd),
            (36, 0x11223344),
            (40, machine.cpu.read_u32(ESP).unwrap()),
            (44, 79),
            (48, 91),
            (52, 77),
        ] {
            assert_eq!(machine.cpu.read_u32(output + offset).unwrap(), expected);
        }
        assert_eq!(word(&machine, output + 56), control);
        assert_eq!(word(&machine, output + 58), 0x2904); // TOP5, preserved C0+ZE.
    }
    assert_eq!(machine.cpu.fpu_environment().unwrap().mxcsr, before.mxcsr);

    // Rejected controls/domain/range/non-binary64/empty operands leave live
    // 80-bit registers, lower values and module error cells intact.
    for (base, exponent, control, extended_operand, empty) in [
        (2.0f64, 0.5f64, 0x067fu16, false, false),
        (2.0, 0.5, 0x007f, false, false),
        (-2.0, 0.5, 0x027f, false, false),
        (2.0, 1024.0, 0x027f, false, false),
        (2.0, -1100.0, 0x027f, false, false),
        (f64::INFINITY, 2.0, 0x027f, false, false),
        (2.0, 0.5, 0x027f, true, false),
        (2.0, 0.5, 0x027f, false, true),
    ] {
        let mut machine = fixture(cpu());
        machine.cpu.write(CONTROL, &control.to_le_bytes()).unwrap();
        machine.cpu.write(INPUT, &base.to_le_bytes()).unwrap();
        machine
            .cpu
            .write(INPUT + 8, &exponent.to_le_bytes())
            .unwrap();
        let entry = machine.resolve_import("msvcr90.dll", "_CIpow").unwrap();
        let mut code = Asm::new(CODE);
        seed(&mut machine, &mut code, "msvcr90.dll");
        absolute(&mut code, &[0xd9, 0x2d], CONTROL);
        if !empty {
            lower(&mut code);
            absolute(&mut code, &[0xdd, 0x05], INPUT);
        }
        absolute(
            &mut code,
            if extended_operand {
                &[0xdb, 0x2d]
            } else {
                &[0xdd, 0x05]
            },
            if extended_operand { LOWER } else { INPUT + 8 },
        );
        code.call(entry).ret();
        let result = execute(&mut machine, code);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        let snapshot = machine.cpu.capture_architecture().unwrap();
        let count = if empty { 1 } else { 4 };
        let expected = [
            if extended_operand {
                EXTENDED
            } else {
                X87Register(x87::encode_f64(exponent))
            },
            X87Register(x87::encode_f64(base)),
            X87Register(x87::encode_f64(-7.5)),
            EXTENDED,
        ];
        for (logical, value) in expected.into_iter().take(count).enumerate() {
            let physical = snapshot.x87.physical_index(logical);
            assert_eq!(snapshot.x87.regs[physical], value);
            assert_ne!(snapshot.x87.physical_tag(physical), 3);
        }
        assert_eq!(snapshot.x87.control, control);
        let errno = machine.cpu.read_u32(ERRNO).unwrap();
        assert_eq!(machine.cpu.read_u32(errno).unwrap(), 79);
        assert_eq!(machine.cpu.read_u32(errno + 4).unwrap(), 91);
        let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.warnings.len(), 1);
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|(api, _)| api == "msvcr90.dll!_CIpow")
        );
    }
}

#[test]
fn typed_x87_power_preserves_zero_word_abi_lower_payloads_flags_and_failure_atomicity() {
    guest(|| UnicornCpu::new(PARK_ADDR).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, false).unwrap());
}
