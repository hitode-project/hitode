//! VC80 oracle: crt-round-native/probe.c, source SHA256
//! FFFAF19A1E0DE2ACD6F263F4D4A042974BA2133EA5B60E2C4BAC6631FD534A33.
//! The final 70.0 case is the actual guest operand, with an exact integral result.
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
const LOWER: u32 = DATA + 7000;
const CONTROL: u32 = LOWER + 16;
const ERRNO: u32 = CONTROL + 8;
const ESP: u32 = ERRNO + 4;
const NAMES: [&str; 3] = ["floor", "ceil", "fabs"];
const INPUTS: [u64; 12] = [
    0,
    SIGN,
    0x4004000000000000,
    0xc004000000000000,
    0x4340000000000000,
    1,
    SIGN | 1,
    0x7ff0000000000000,
    0xfff0000000000000,
    0x7ff8000000000042,
    0xfff8000000000042,
    0x4051800000000000,
];
const GOLDEN: [[u64; 12]; 3] = [
    [
        0,
        SIGN,
        0x4000000000000000,
        0xc008000000000000,
        0x4340000000000000,
        0,
        0xbff0000000000000,
        0x7ff0000000000000,
        0xfff0000000000000,
        0x7ff8000000000042,
        0xfff8000000000042,
        0x4051800000000000,
    ],
    [
        0,
        SIGN,
        0x4008000000000000,
        0xc000000000000000,
        0x4340000000000000,
        0x3ff0000000000000,
        SIGN,
        0x7ff0000000000000,
        0xfff0000000000000,
        0x7ff8000000000042,
        0xfff8000000000042,
        0x4051800000000000,
    ],
    [
        0,
        0,
        0x4004000000000000,
        0x4004000000000000,
        0x4340000000000000,
        1,
        1,
        0x7ff0000000000000,
        0x7ff0000000000000,
        0x7ff8000000000042,
        0xfff8000000000042,
        0x4051800000000000,
    ],
];

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        for name in NAMES {
            let signature = adv_abi::signature::lookup(module, name).unwrap();
            assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 2 }));
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                signature.conv.unwrap()
            );
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-round-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
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
    memory(code, &[0xdd, 0x05], LOWER); // fld qword
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
    code.push_imm((input >> 32) as u32).push_imm(input as u32);
    code.bytes.push(0xb8);
    code.bytes.extend_from_slice(&0xaabbccddu32.to_le_bytes());
    code.bytes.push(0xba);
    code.bytes.extend_from_slice(&0x11223344u32.to_le_bytes());
    code.call(entry).add_esp(8);
}
fn execute<B: Backend>(machine: &mut Machine<B>, code: Asm) -> RunResult {
    assert!(code.bytes.len() < 16384);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("typed cdecl binary64 rounding", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}

fn guest<B: Backend>(cpu: impl Fn() -> B) {
    let mut machine = fixture(cpu());
    let before = machine.cpu.fpu_environment().unwrap();
    let controls = [0x027fu16, 0x0a7f, 0x0f7f];
    for (index, control) in controls.into_iter().enumerate() {
        machine
            .cpu
            .write(CONTROL + index as u32 * 2, &control.to_le_bytes())
            .unwrap();
    }
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    code.mov_mem_eax(ESP);
    let mut outputs = Vec::new();
    for (module_index, module) in MODULES.into_iter().enumerate() {
        memory(&mut code, &[0xd9, 0x2d], CONTROL + module_index as u32 * 2); // fldcw
        for (operation, name) in NAMES.into_iter().enumerate() {
            let entry = machine.resolve_import(module, name).unwrap();
            for (index, input) in INPUTS.into_iter().enumerate() {
                if operation == 2 && matches!(index, 5 | 6) {
                    continue; // Explicit unsupported DE status profile below.
                }
                let output = DATA + outputs.len() as u32 * 64;
                seed(&mut machine, &mut code, module);
                lower(&mut code);
                argument(&mut code, entry, input);
                code.mov_mem_eax(output + 24);
                memory(&mut code, &[0x89, 0x15], output + 28); // EDX is not a floating return lane.
                for offset in [0, 8, 16] {
                    memory(&mut code, &[0xdd, 0x1d], output + offset); // fstp qword
                }
                code.bytes.extend_from_slice(&[0x89, 0xe0]);
                code.mov_mem_eax(output + 32);
                code.mov_eax_mem(ERRNO)
                    .mov_eax_ptr_eax()
                    .mov_mem_eax(output + 36);
                code.mov_eax_mem(ERRNO)
                    .bytes
                    .extend_from_slice(&[0x8b, 0x40, 4]);
                code.mov_mem_eax(output + 40);
                let get_error = machine
                    .resolve_import("kernel32.dll", "GetLastError")
                    .unwrap();
                code.call(get_error).mov_mem_eax(output + 44);
                memory(&mut code, &[0xd9, 0x3d], output + 48); // fnstcw
                outputs.push((output, operation, index, controls[module_index]));
            }
        }
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    let result = execute(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (output, operation, index, control) in outputs {
        assert_eq!(
            bits(&machine, output),
            GOLDEN[operation][index],
            "{} input {index}",
            NAMES[operation]
        );
        assert_eq!(bits(&machine, output + 8), (-7.5f64).to_bits());
        assert_eq!(bits(&machine, output + 16), 3.25f64.to_bits());
        for (offset, expected) in [
            (24, 0xaabbccdd),
            (28, 0x11223344),
            (32, machine.cpu.read_u32(ESP).unwrap()),
            (36, if matches!(index, 9 | 10) { 33 } else { 79 }),
            (40, 91),
            (44, 77),
        ] {
            assert_eq!(machine.cpu.read_u32(output + offset).unwrap(), expected);
        }
        let mut word = [0; 2];
        machine.cpu.read(output + 48, &mut word).unwrap();
        assert_eq!(u16::from_le_bytes(word), control);
    }
    let after = machine.cpu.fpu_environment().unwrap();
    assert_eq!(after.x87_control, controls[2]);
    assert_eq!(after.mxcsr, before.mxcsr);

    // Unsupported FP side effects fail without consuming pre-existing ST0 or
    // publishing a double, errno, or a made-up CRT invalid callback.
    for (name, input) in [("floor", 0x7ff0000000000042), ("fabs", 1)] {
        let mut machine = fixture(cpu());
        let entry = machine.resolve_import("msvcr80.dll", name).unwrap();
        let mut code = Asm::new(CODE);
        seed(&mut machine, &mut code, "msvcr80.dll");
        lower(&mut code);
        argument(&mut code, entry, input);
        code.ret();
        let result = execute(&mut machine, code);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert_eq!(
            machine.cpu.pop_f64().unwrap().to_bits(),
            (-7.5f64).to_bits()
        );
        assert_eq!(machine.cpu.pop_f64().unwrap().to_bits(), 3.25f64.to_bits());
        let errno = machine.cpu.read_u32(ERRNO).unwrap();
        assert_eq!(machine.cpu.read_u32(errno).unwrap(), 79);
        let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.warnings.len(), 1);
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|(api, _)| api == &format!("msvcr80.dll!{name}"))
        );
    }
    // The typed decoder preflights both DWORDs before touching the FP owner.
    let mut machine = fixture(cpu());
    let entry = machine.resolve_import("msvcr80.dll", "floor").unwrap();
    machine.cpu.map(DATA + 8192, 4096, Prot::WRITE).unwrap();
    machine
        .cpu
        .write(DATA + 8184, &PARK_ADDR.to_le_bytes())
        .unwrap();
    machine.cpu.write(DATA + 8188, &0u32.to_le_bytes()).unwrap();
    machine
        .cpu
        .write(DATA + 8192, &0x40518000u32.to_le_bytes())
        .unwrap();
    let mut code = Asm::new(CODE);
    lower(&mut code);
    code.bytes.push(0xbc); // mov esp, deliberately truncated readable args.
    code.bytes.extend_from_slice(&(DATA + 8184).to_le_bytes());
    code.jmp(entry);
    let result = execute(&mut machine, code);
    assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
    assert_eq!(
        machine.cpu.pop_f64().unwrap().to_bits(),
        (-7.5f64).to_bits()
    );
    assert_eq!(machine.cpu.pop_f64().unwrap().to_bits(), 3.25f64.to_bits());
}

#[test]
fn typed_scalar_rounding_preserves_binary64_abi_stack_controls_and_error_owner() {
    guest(|| UnicornCpu::new(PARK_ADDR).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, false).unwrap());
}
