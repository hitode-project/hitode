//! Authored VC80/90 oracle: crt-log-native/probe.c, SHA256
//! 0A2237B76D0CAB51E11C66C37B1DE7DDE8B6A8DE31CB472CB7D19BE222B0B702.
//! Natural log oracle: crt-ln-native/probe.c, SHA256
//! ABD154E44ABA0574CF41DC4006BC789ABD690DF1F86A0F61C90EA32FD16ED3A6.
//! Isolated VC90 C1 seed: crt-ln-c1-native/probe.c, SHA256
//! EFB305F32171FBCB66E305161238770ABB2867B3F450BF82743BA36C4FBABFFD.
//! Numeric output tolerates two binary64 ULPs; ABI, lower payloads, controls,
//! errors, C1 retirement and atomic rejection are checked exactly.
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
const INPUT: u32 = DATA + 8192;
const LOWER: u32 = DATA + 12288;
const CONTROL: u32 = LOWER + 32;
const ERRNO: u32 = CONTROL + 8;
const ESP: u32 = ERRNO + 4;
const SAVED: u32 = DATA + 13312;
const EXTENDED: X87Register = X87Register([1, 0, 0, 0, 0, 0, 0, 0x80, 0xff, 0x3f]);
const GOLDEN: [(f64, u64); 5] = [
    (0.1, 0xbff0000000000000),
    (0.5, 0xbfd34413509f79ff),
    (1.0, 0),
    (10.0, 0x3ff0000000000000),
    (100.0, 0x4000000000000000),
];
const LN_GOLDEN: [(f64, u64); 3] = [
    (2.0, 0x3fe62e42fefa39ef),
    (1024.0, 0x401bb9d3beb8c86b),
    (2048.0, 0x401e7f9c1e980fa9),
];
// These are the six measured FILD(volume)/FDIV(1000.0), PC64 inputs.
const VOLUME: [(u32, u64, i64); 6] = [
    (1, 0xc008000000000000, -9960),
    (10, 0xc000000000000000, -6640),
    (100, 0xbff0000000000000, -3320),
    (700, 0xbfc3d3d3d21ccf04, -514),
    (999, 0xbf3c79e5dbf022b6, -1),
    (1000, 0, 0),
];

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        for name in ["_CIlog10", "_CIlog"] {
            let signature = adv_abi::signature::lookup(module, name).unwrap();
            assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 0 }));
            assert_eq!(signature.return_kind, AbiKind::Void);
            assert!(signature.parameters.is_empty());
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-log-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
    for (address, bytes) in [
        (LOWER, EXTENDED.0.to_vec()),
        (LOWER + 16, (-7.5f64).to_le_bytes().to_vec()),
        (INPUT, 1000.0f64.to_le_bytes().to_vec()),
        (INPUT + 8, 33.2f64.to_le_bytes().to_vec()),
        (INPUT + 16, 100.0f64.to_le_bytes().to_vec()),
        (CONTROL, 0x027fu16.to_le_bytes().to_vec()),
        (CONTROL + 2, 0x037fu16.to_le_bytes().to_vec()),
    ] {
        machine.cpu.write(address, &bytes).unwrap();
    }
    machine
}

fn absolute(code: &mut Asm, opcode: &[u8], address: u32) {
    code.bytes.extend_from_slice(opcode);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}
fn lower(code: &mut Asm) {
    absolute(code, &[0xdb, 0x2d], LOWER);
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
fn sticky(code: &mut Asm) {
    // Existing ZE and C0 survive the native routine. FDIV may additionally set
    // PE and C1; the logarithm retires only C1 on successful return.
    code.bytes.extend_from_slice(&[
        0xdb, 0xe2, // fnclex
        0xd9, 0xe8, 0xd9, 0xee, 0xde, 0xf9, 0xdd, 0xd8, // 1/0, discard.
        0xd9, 0xe8, 0xd9, 0xee, 0xde, 0xd9, // 0<1, fcompp.
    ]);
}
fn execute<B: Backend>(machine: &mut Machine<B>, code: Asm) -> RunResult {
    assert!(code.bytes.len() < 16384);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("zero-word x87 intrinsic logarithm", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}

fn guest<B: Backend>(cpu: impl Fn() -> B) {
    let mut machine = fixture(cpu());
    let before = machine.cpu.fpu_environment().unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(ESP);
    let mut outputs = Vec::new();
    for (module_index, module) in MODULES.into_iter().enumerate() {
        for (name, golden) in [
            ("_CIlog10", GOLDEN.as_slice()),
            ("_CIlog", LN_GOLDEN.as_slice()),
        ] {
            let entry = machine.resolve_import(module, name).unwrap();
            let volume_count = if name == "_CIlog10" { VOLUME.len() } else { 0 };
            for case in 0..golden.len() + volume_count {
                let index = outputs.len() as u32;
                let output = DATA + index * 96;
                let input = INPUT + 256 + index * 16;
                let volume = (case >= golden.len()).then(|| VOLUME[case - golden.len()]);
                let control = if volume.is_some() || module_index == 1 {
                    0x037f
                } else {
                    0x027f
                };
                seed(&mut machine, &mut code, module);
                absolute(
                    &mut code,
                    &[0xd9, 0x2d],
                    CONTROL + u32::from(control == 0x037f) * 2,
                );
                sticky(&mut code);
                lower(&mut code);
                let expected = if let Some((value, expected, _)) = volume {
                    machine.cpu.write_u32(input, value).unwrap();
                    absolute(&mut code, &[0xdb, 0x05], input); // FILD integer volume.
                    absolute(&mut code, &[0xdc, 0x35], INPUT); // FDIV 1000.0, PC64.
                    expected
                } else {
                    let (value, expected) = golden[case];
                    machine.cpu.write(input, &value.to_le_bytes()).unwrap();
                    absolute(&mut code, &[0xdd, 0x05], input);
                    expected
                };
                if name == "_CIlog" && case == 0 {
                    // Native ln(2) clears a seeded C1 but preserves C0+ZE.
                    // Change only C1 in the neutral architectural save area.
                    absolute(&mut code, &[0x0f, 0xae, 0x05], SAVED);
                    absolute(&mut code, &[0x66, 0x81, 0x0d], SAVED + 2);
                    code.bytes.extend_from_slice(&0x0200u16.to_le_bytes());
                    absolute(&mut code, &[0x0f, 0xae, 0x0d], SAVED);
                }
                absolute(&mut code, &[0xdd, 0x3d], output + 72); // before flags.
                code.bytes.push(0xb8);
                code.bytes.extend_from_slice(&0xaabbccddu32.to_le_bytes());
                code.bytes.push(0xba);
                code.bytes.extend_from_slice(&0x11223344u32.to_le_bytes());
                code.call(entry); // Genuine void/cdecl0, result through ST0.
                code.mov_mem_eax(output + 32);
                absolute(&mut code, &[0x89, 0x15], output + 36);
                absolute(&mut code, &[0xdd, 0x3d], output + 58);
                if volume.is_some() {
                    absolute(&mut code, &[0xdd, 0x15], output); // FST, keep returned ST0.
                    absolute(&mut code, &[0xdc, 0x0d], INPUT + 8); // FMUL 33.2.
                    absolute(&mut code, &[0xdc, 0x0d], INPUT + 16); // FMUL 100.0.
                    absolute(&mut code, &[0xdd, 0x0d], output + 64); // FISTTP int64.
                } else {
                    absolute(&mut code, &[0xdd, 0x1d], output);
                }
                absolute(&mut code, &[0xdd, 0x1d], output + 8);
                absolute(&mut code, &[0xdb, 0x3d], output + 16);
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
                outputs.push((output, expected, control, volume));
            }
        }
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    let result = execute(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (output, expected, control, volume) in outputs {
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
        assert_eq!(
            word(&machine, output + 58),
            word(&machine, output + 72) & !0x0200
        );
        assert_eq!(word(&machine, output + 58) & !0x0020, 0x2904);
        if let Some((_, _, expected)) = volume {
            assert_eq!(bits(&machine, output + 64) as i64, expected);
        }
    }
    assert_eq!(machine.cpu.fpu_environment().unwrap().mxcsr, before.mxcsr);

    // Every rejected call snapshots the live architectural stack before the
    // call. No pop/re-encode can destroy a lower or failing 80-bit payload.
    for name in ["_CIlog10", "_CIlog"] {
        for (input, control) in [
            (Some(X87Register(x87::encode_f64(0.5))), 0x067f),
            (Some(X87Register(x87::encode_f64(0.5))), 0x007f),
            (Some(X87Register(x87::encode_f64(0.5))), 0x027e),
            (Some(X87Register(x87::encode_f64(0.0))), 0x027f),
            (Some(X87Register(x87::encode_f64(-1.0))), 0x027f),
            (Some(X87Register(x87::encode_f64(f64::INFINITY))), 0x027f),
            (
                Some(X87Register(x87::encode_f64(f64::from_bits(1)))),
                0x027f,
            ),
            (Some(EXTENDED), 0x037f), // Rounds to 1, but is not exactly 1.
            (Some(X87Register::from_parts((1 << 63) + 1, 0x4000)), 0x037f),
            (None, 0x027f),
        ] {
            let mut machine = fixture(cpu());
            machine
                .cpu
                .write(CONTROL, &u16::to_le_bytes(control))
                .unwrap();
            let mut code = Asm::new(CODE);
            seed(&mut machine, &mut code, "msvcr90.dll");
            absolute(&mut code, &[0xd9, 0x2d], CONTROL);
            if let Some(input) = input {
                machine.cpu.write(INPUT + 256, &input.0).unwrap();
                lower(&mut code);
                absolute(&mut code, &[0xdb, 0x2d], INPUT + 256);
            }
            absolute(&mut code, &[0x0f, 0xae, 0x05], SAVED); // FXSAVE, no stack change.
            let entry = machine.resolve_import("msvcr90.dll", name).unwrap();
            code.call(entry).ret();
            let result = execute(&mut machine, code);
            assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
            let snapshot = machine.cpu.capture_architecture().unwrap();
            assert_eq!(snapshot.x87.control, word(&machine, SAVED));
            assert_eq!(snapshot.x87.status, word(&machine, SAVED + 2));
            let count = if input.is_some() { 3 } else { 0 };
            for logical in 0..8 {
                let physical = snapshot.x87.physical_index(logical);
                if logical < count {
                    let mut raw = [0; 10];
                    machine
                        .cpu
                        .read(SAVED + 32 + logical as u32 * 16, &mut raw)
                        .unwrap();
                    assert_eq!(snapshot.x87.regs[physical].0, raw);
                    assert_eq!(snapshot.x87.physical_tag(physical), X87Register(raw).tag());
                } else {
                    assert_eq!(snapshot.x87.physical_tag(physical), 3);
                }
            }
            let errno = machine.cpu.read_u32(ERRNO).unwrap();
            assert_eq!(machine.cpu.read_u32(errno).unwrap(), 79);
            assert_eq!(machine.cpu.read_u32(errno + 4).unwrap(), 91);
            let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
            assert_eq!(diagnostics.warnings.len(), 1);
            assert!(
                diagnostics
                    .warnings
                    .keys()
                    .any(|(api, _)| api == &format!("msvcr90.dll!{name}"))
            );
        }
    }
}

#[test]
fn typed_logarithm_preserves_register_abi_volume_results_flags_and_atomic_failures() {
    guest(|| UnicornCpu::new(PARK_ADDR).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, false).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, true).unwrap());
}
