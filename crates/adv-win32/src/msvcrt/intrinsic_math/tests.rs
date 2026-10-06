//! Authored VC80 PC53 evidence: crt-intrinsic-native/native.jsonl. The small
//! transcendental tolerance is intentional: this is not a bit-exact CRT claim.
use super::*;
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const INPUT: u32 = DATA + 4096;
const LOWER: u32 = INPUT + 64;
const CONTROL: u32 = INPUT + 80;
const ESP_INITIAL: u32 = INPUT + 84;
const LAST_ERROR: u32 = 0x12345678;
const NAMES: [&str; 4] = ["_CItan", "_CIsin", "_CIcos", "_CIsqrt"];
const INPUTS: [u64; 5] = [
    0,
    1 << 63,
    0x3ff0000000000000,
    0x4000000000000000,
    0x4022000000000000,
];
const GOLDEN: [[u64; 5]; 4] = [
    [
        0,
        1 << 63,
        0x3ff8eb245cbee3a6,
        0xc0017af62e0950f8,
        0xbfdcf2bd61315c1c,
    ],
    [
        0,
        1 << 63,
        0x3feaed548f090cee,
        0x3fed18f6ead1b446,
        0x3fda6026360c2f91,
    ],
    [
        0x3ff0000000000000,
        0x3ff0000000000000,
        0x3fe14a280fb5068c,
        0xbfdaa22657537205,
        0xbfed27faa6a6196b,
    ],
    [
        0,
        1 << 63,
        0x3ff0000000000000,
        0x3ff6a09e667f3bcd,
        0x4008000000000000,
    ],
];

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        for name in NAMES {
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                Conv::Cdecl { argc: 0 }
            );
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-intrinsic-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 8192, Prot::RX).unwrap();
    machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
    for (index, bits) in INPUTS.into_iter().enumerate() {
        machine
            .cpu
            .write(INPUT + index as u32 * 8, &bits.to_le_bytes())
            .unwrap();
    }
    machine.cpu.write(LOWER, &3.25f64.to_le_bytes()).unwrap();
    machine
        .cpu
        .write(LOWER + 8, &(-7.5f64).to_le_bytes())
        .unwrap();
    machine
}

fn absolute(code: &mut Asm, opcode: &[u8], address: u32) {
    code.bytes.extend_from_slice(opcode);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}
fn load_stack(code: &mut Asm, input: u32) {
    absolute(code, &[0xdd, 0x05], LOWER); // fld qword
    absolute(code, &[0xdd, 0x05], LOWER + 8);
    absolute(code, &[0xdd, 0x05], input);
}
fn bits<B: Backend>(machine: &Machine<B>, address: u32) -> u64 {
    let mut bytes = [0; 8];
    machine.cpu.read(address, &mut bytes).unwrap();
    u64::from_le_bytes(bytes)
}

fn success<B: Backend>(cpu: B) {
    let mut machine = fixture(cpu);
    machine
        .cpu
        .write(CONTROL, &0x027fu16.to_le_bytes())
        .unwrap();
    machine
        .cpu
        .write(CONTROL + 2, &0x037fu16.to_le_bytes())
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let exit = machine
        .resolve_import("kernel32.dll", "ExitProcess")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe5, 0x89, 0xe0]); // EBP/record = initial ESP.
    code.mov_mem_eax(ESP_INITIAL);
    let mut outputs = Vec::new();
    for (module_index, module) in MODULES.into_iter().enumerate() {
        let errno = machine.resolve_import(module, "_errno").unwrap();
        code.call(errno).bytes.extend_from_slice(&[0xc7, 0x00]);
        code.bytes.extend_from_slice(&79u32.to_le_bytes());
        code.push_imm(LAST_ERROR).call(set_error);
        // Both supported precision modes share the one guest control owner.
        absolute(
            &mut code,
            &[0xd9, 0x2d],
            CONTROL + if module_index == 1 { 2 } else { 0 },
        );
        for (operation, name) in NAMES.into_iter().enumerate() {
            let entry = machine.resolve_import(module, name).unwrap();
            for (input_index, _) in INPUTS.into_iter().enumerate() {
                let out = DATA + outputs.len() as u32 * 40;
                load_stack(&mut code, INPUT + input_index as u32 * 8);
                code.bytes.extend_from_slice(&[0xb8]); // Ensure F64 return doesn't replace EAX/EDX.
                code.bytes.extend_from_slice(&0xaabbccddu32.to_le_bytes());
                code.bytes.extend_from_slice(&[0xba]);
                code.bytes.extend_from_slice(&0x11223344u32.to_le_bytes());
                code.call(entry).mov_mem_eax(out + 24);
                absolute(&mut code, &[0x89, 0x15], out + 28); // mov [out+28],edx
                for offset in [0, 8, 16] {
                    absolute(&mut code, &[0xdd, 0x1d], out + offset); // fstp qword
                }
                code.bytes.extend_from_slice(&[0x89, 0xe0]);
                code.mov_mem_eax(out + 32);
                outputs.push((out, operation, input_index));
            }
        }
        let out = DATA + 3000 + module_index as u32 * 16;
        code.call(errno).bytes.extend_from_slice(&[0x8b, 0x00]);
        code.mov_mem_eax(out).call(get_error).mov_mem_eax(out + 4);
        absolute(&mut code, &[0xd9, 0x3d], out + 8); // fnstcw
    }
    code.push_imm(0).call(exit).ret();
    assert!(code.bytes.len() < 8192);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("x87 intrinsic cdecl0", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (out, operation, input_index) in outputs {
        let actual = bits(&machine, out);
        let expected = GOLDEN[operation][input_index];
        if operation == 3 || input_index < 2 {
            assert_eq!(
                actual, expected,
                "{} input{}",
                NAMES[operation], input_index
            );
        } else {
            assert!(
                actual.abs_diff(expected) <= 1,
                "{} input{}: {actual:016x}/{expected:016x}",
                NAMES[operation],
                input_index
            );
        }
        assert_eq!(bits(&machine, out + 8), (-7.5f64).to_bits());
        assert_eq!(bits(&machine, out + 16), 3.25f64.to_bits());
        assert_eq!(machine.cpu.read_u32(out + 24).unwrap(), 0xaabbccdd);
        assert_eq!(machine.cpu.read_u32(out + 28).unwrap(), 0x11223344);
        assert_eq!(
            machine.cpu.read_u32(out + 32).unwrap(),
            machine.cpu.read_u32(ESP_INITIAL).unwrap()
        );
    }
    for index in 0..3 {
        let out = DATA + 3000 + index * 16;
        assert_eq!(machine.cpu.read_u32(out).unwrap(), 79);
        assert_eq!(machine.cpu.read_u32(out + 4).unwrap(), LAST_ERROR);
        assert_eq!(
            machine.cpu.read_u16(out + 8).unwrap(),
            if index == 1 { 0x037f } else { 0x027f }
        );
    }
    assert!(
        machine
            .ext
            .get_or_default::<adv_abi::ApiDiagnostics>()
            .warnings
            .is_empty()
    );
}

fn rejection<B: Backend>(cpu: B, name: &str, input: Option<f64>, control: u16, reason: &str) {
    let mut machine = fixture(cpu);
    machine.cpu.write(CONTROL, &control.to_le_bytes()).unwrap();
    if let Some(input) = input {
        machine.cpu.write(INPUT, &input.to_le_bytes()).unwrap();
    }
    let entry = machine.resolve_import("msvcr90.dll", name).unwrap();
    let mut code = Asm::new(CODE);
    absolute(&mut code, &[0xd9, 0x2d], CONTROL);
    if input.is_some() {
        load_stack(&mut code, INPUT);
    }
    code.call(entry).mov_mem_imm(DATA, 0xbad).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("unsupported intrinsic profile", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Failed(ref error) if error.message.contains(reason)),
        "{result:?}"
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0, "no fake return");
    if let Some(input) = input {
        assert_eq!(
            machine.cpu.pop_f64().unwrap().to_bits(),
            input.to_bits(),
            "rejected ST0 preserved"
        );
        assert_eq!(machine.cpu.pop_f64(), Some(-7.5));
        assert_eq!(machine.cpu.pop_f64(), Some(3.25));
    }
    assert!(machine.cpu.pop_f64().is_none());
    assert_eq!(machine.cpu.fpu_environment().unwrap().x87_control, control);
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert_eq!(diagnostics.warnings.len(), 1);
    let ((api, reported_reason), &count) = diagnostics.warnings.iter().next().unwrap();
    assert_eq!(api, &format!("msvcr90.dll!{name}"));
    assert!(reported_reason.contains(reason));
    assert_eq!(count, 1);
}

fn guest<B: Backend>(mut cpu: impl FnMut() -> B) {
    success(cpu());
    for (name, input, control, reason) in [
        ("_CIsqrt", Some(-1.0), 0x027f, "negative square-root"),
        ("_CItan", Some(f64::INFINITY), 0x027f, "nonfinite"),
        ("_CIsin", Some(1.0), 0x027e, "unmasked"),
        ("_CIcos", Some(1.0), 0x067f, "non-nearest"),
        ("_CIsqrt", Some(1.0), 0x007f, "precision"),
        ("_CIsin", None, 0x027f, "stack is empty"),
    ] {
        rejection(cpu(), name, input, control, reason);
    }
}

#[test]
fn vc80_x87_stack_cdecl_results_and_profile_failures() {
    guest(|| JitCpu::new(PARK_ADDR, false).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, true).unwrap());
    guest(|| UnicornCpu::new(PARK_ADDR).unwrap());
}
