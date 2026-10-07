//! VC80 scratch oracle: crt-path-native/probe.c, SHA256
//! 267785BB87B72767F6C04A7FF27A061CA1E5F6029A96E4E989B3FF6BE92279C1.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const HANDLER: u32 = CODE + 3072;
const DATA: u32 = 0x410000;
const SOURCE_A: u32 = DATA + 0x100;
const SOURCE_W: u32 = DATA + 0x200;
const LOCALE: u32 = DATA + 0x300;
const ERRNO: u32 = DATA + 0x400;
const COUNTER: u32 = ERRNO + 4;
const SEEN_ERRNO: u32 = COUNTER + 4;
const SEEN_OUTPUT: u32 = SEEN_ERRNO + 4;
const DRIVE: u32 = DATA + 0x800;
const DIR: u32 = DRIVE + 6;
const NAME: u32 = DIR + 512;
const EXT: u32 = NAME + 512;
const ROWS: u32 = DATA + 0x1000;
const WRITE_ONLY: u32 = DATA + 16384;
const READ_ONLY: u32 = WRITE_ONLY + 4096;

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        for name in ["_splitpath_s", "_wsplitpath_s"] {
            let signature = adv_abi::signature::lookup(module, name).unwrap();
            assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 9 }));
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                signature.conv.unwrap()
            );
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-path-tests");
    let mut config = crate::Win32Config::new(&root, "fixture.exe", &root);
    config.os.codepage = 936;
    crate::Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
    machine.cpu.map(WRITE_ONLY, 4096, Prot::WRITE).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine
        .cpu
        .write(SOURCE_A, b"C:\\dir.name/mixed\\a\x81\x5cb.tar.gz\0")
        .unwrap();
    machine
        .cpu
        .write(SOURCE_W, &wide("C:\\dir.name/mixed\\银河.tar.gz"))
        .unwrap();
    machine
        .cpu
        .write(LOCALE, b"English_United States.1252\0")
        .unwrap();
    machine
        .cpu
        .write(DRIVE, &vec![0x5a; (EXT + 512 - DRIVE) as usize])
        .unwrap();
    let mut handler = Asm::new(HANDLER);
    handler.bytes.extend_from_slice(&[0xff, 0x05]);
    handler.bytes.extend_from_slice(&COUNTER.to_le_bytes());
    handler
        .mov_eax_mem(ERRNO)
        .mov_eax_ptr_eax()
        .mov_mem_eax(SEEN_ERRNO);
    for (index, address) in [DRIVE, DIR, NAME, EXT].into_iter().enumerate() {
        handler
            .mov_eax_mem(address)
            .mov_mem_eax(SEEN_OUTPUT + index as u32 * 4);
    }
    handler.ret();
    machine.cpu.write(HANDLER, &handler.bytes).unwrap();
    machine
}
fn wide(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}
fn call(code: &mut Asm, api: u32, args: &[u32]) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(api).add_esp((args.len() * 4) as u8);
}
fn immediate(code: &mut Asm, address: u32, value: u32) {
    code.bytes.push(0xb8);
    code.bytes.extend_from_slice(&value.to_le_bytes());
    code.mov_mem_eax(address);
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
    immediate(code, COUNTER, 0);
    code.push_imm(77).call(set_error);
}
fn capture<B: Backend>(machine: &mut Machine<B>, code: &mut Asm, row: u32) {
    code.mov_mem_eax(row);
    code.mov_eax_mem(ERRNO)
        .mov_eax_ptr_eax()
        .mov_mem_eax(row + 4);
    code.mov_eax_mem(ERRNO)
        .bytes
        .extend_from_slice(&[0x8b, 0x40, 4]);
    code.mov_mem_eax(row + 8);
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    code.call(get_error).mov_mem_eax(row + 12);
    code.mov_eax_mem(COUNTER).mov_mem_eax(row + 16);
    code.mov_eax_mem(SEEN_ERRNO).mov_mem_eax(row + 20);
    for index in 0..4 {
        code.mov_eax_mem(SEEN_OUTPUT + index * 4)
            .mov_mem_eax(row + 24 + index * 4);
    }
}
fn execute<B: Backend>(machine: &mut Machine<B>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    assert!(code.bytes.len() < (HANDLER - CODE) as usize);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored secure CRT path split", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}
fn parts(source: u32, addresses: [u32; 4], capacities: [u32; 4]) -> [u32; 9] {
    [
        source,
        addresses[0],
        capacities[0],
        addresses[1],
        capacities[1],
        addresses[2],
        capacities[2],
        addresses[3],
        capacities[3],
    ]
}
fn read<B: Backend>(machine: &Machine<B>, pointer: u32, bytes: usize) -> Vec<u8> {
    let mut output = vec![0; bytes];
    machine.cpu.read(pointer, &mut output).unwrap();
    output
}
fn row<B: Backend>(machine: &Machine<B>, address: u32, expected: [u32; 5]) {
    for (index, expected) in expected.into_iter().enumerate() {
        assert_eq!(
            machine.cpu.read_u32(address + index as u32 * 4).unwrap(),
            expected
        );
    }
}

fn success<B: Backend>(cpu: B) {
    let mut machine = fixture(cpu);
    let mut code = Asm::new(CODE);
    let mut cases = Vec::new();
    for (module_index, module) in MODULES.into_iter().enumerate() {
        // LC_CTYPE1252 must not turn the configured default MB codepage936
        // into single-byte parsing; the 81/5C pair contains a trail backslash.
        let locale = machine.resolve_import(module, "setlocale").unwrap();
        call(&mut code, locale, &[2, LOCALE]);
        for (wide_index, is_wide) in [false, true].into_iter().enumerate() {
            let output = ROWS + (module_index * 2 + wide_index) as u32 * 1792;
            let width = if is_wide { 2 } else { 1 };
            let addresses = [
                output + 64,
                output + 64 + 3 * width,
                output + 64 + 259 * width,
                output + 64 + 515 * width,
            ];
            machine
                .cpu
                .write(output + 64, &vec![0x5a; (3 + 3 * 256) * width as usize])
                .unwrap();
            let api = machine
                .resolve_import(
                    module,
                    if is_wide {
                        "_wsplitpath_s"
                    } else {
                        "_splitpath_s"
                    },
                )
                .unwrap();
            seed(&mut machine, &mut code, module);
            code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
            code.mov_mem_eax(output + 40);
            call(
                &mut code,
                api,
                &parts(
                    if is_wide { SOURCE_W } else { SOURCE_A },
                    addresses,
                    [3, 256, 256, 256],
                ),
            );
            capture(&mut machine, &mut code, output);
            code.bytes.extend_from_slice(&[0x89, 0xe0]);
            code.mov_mem_eax(output + 44);
            cases.push((output, addresses, is_wide));
        }
    }
    let api = machine
        .resolve_import("msvcr90.dll", "_wsplitpath_s")
        .unwrap();
    // Paired NULL/zero outputs are optional, not invalid parameters.
    call(&mut code, api, &parts(SOURCE_W, [0; 4], [0; 4]));
    code.mov_mem_eax(DATA);
    // Valid WRITE-only output supports the complete result without reads.
    call(
        &mut code,
        api,
        &parts(SOURCE_W, [0, 0, WRITE_ONLY, 0], [0, 0, 256, 0]),
    );
    code.mov_mem_eax(DATA + 4);
    let result = execute(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (output, addresses, is_wide) in cases {
        row(&machine, output, [0, 79, 91, 77, 0]);
        assert_eq!(
            machine.cpu.read_u32(output + 40).unwrap(),
            machine.cpu.read_u32(output + 44).unwrap()
        );
        let expected = if is_wide {
            [
                wide("C:"),
                wide("\\dir.name/mixed\\"),
                wide("银河.tar"),
                wide(".gz"),
            ]
        } else {
            [
                b"C:\0".to_vec(),
                b"\\dir.name/mixed\\\0".to_vec(),
                b"a\x81\x5cb.tar\0".to_vec(),
                b".gz\0".to_vec(),
            ]
        };
        for (index, (address, expected)) in addresses.into_iter().zip(expected).enumerate() {
            assert_eq!(read(&machine, address, expected.len()), expected);
            let capacity = if index == 0 { 3 } else { 256 };
            let bytes = capacity * if is_wide { 2 } else { 1 };
            assert!(
                read(
                    &machine,
                    address + expected.len() as u32,
                    bytes - expected.len()
                )
                .iter()
                .all(|&byte| byte == 0x5a)
            );
        }
    }
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 0);
    machine.cpu.protect(WRITE_ONLY, 4096, Prot::RW).unwrap();
    assert_eq!(
        read(&machine, WRITE_ONLY, wide("银河.tar").len()),
        wide("银河.tar")
    );
}

fn failure<B: Backend>(cpu: impl Fn() -> B) {
    let mut machine = fixture(cpu());
    let api = machine
        .resolve_import("msvcr90.dll", "_wsplitpath_s")
        .unwrap();
    let setter = machine
        .resolve_import("msvcr90.dll", "_set_invalid_parameter_handler")
        .unwrap();
    let addresses = [DRIVE, DIR, NAME, EXT];
    let mut code = Asm::new(CODE);
    call(&mut code, setter, &[HANDLER]);
    for (index, args) in [
        parts(0, addresses, [3, 256, 256, 256]),
        parts(SOURCE_W, [0, DIR, NAME, EXT], [3, 256, 256, 256]),
        parts(SOURCE_W, addresses, [0, 256, 256, 256]),
        parts(SOURCE_W, addresses, [3, 256, 2, 256]),
        parts(SOURCE_W, [SOURCE_W, DIR, NAME, EXT], [3, 256, 256, 256]),
    ]
    .into_iter()
    .enumerate()
    {
        seed(&mut machine, &mut code, "msvcr90.dll");
        call(&mut code, api, &args);
        capture(&mut machine, &mut code, ROWS + index as u32 * 64);
    }
    let result = execute(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for index in 0..3 {
        let output = ROWS + index * 64;
        row(&machine, output, [22, 22, 91, 0, 1]);
        assert_eq!(machine.cpu.read_u32(output + 20).unwrap(), 22);
        for offset in [24, 28, 32, 36] {
            assert_eq!(machine.cpu.read_u32(output + offset).unwrap() & 0xffff, 0);
        }
    }
    row(&machine, ROWS + 3 * 64, [34, 34, 91, 77, 0]);
    row(&machine, ROWS + 4 * 64, [40, 40, 91, 77, 0]);
    for address in addresses {
        assert_eq!(read(&machine, address, 2), [0, 0]);
    }
    assert_eq!(read(&machine, DRIVE + 2, 4), wide(":").as_slice());
    assert_eq!(
        read(&machine, DIR + 2, wide("dir.name/mixed\\").len()),
        wide("dir.name/mixed\\")
    );
    assert_eq!(
        read(
            &machine,
            SOURCE_W,
            wide("C:\\dir.name/mixed\\银河.tar.gz").len()
        ),
        wide("C:\\dir.name/mixed\\银河.tar.gz")
    );
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    assert!(
        diagnostics
            .warnings
            .keys()
            .any(|(api, reason)| api == "msvcr90.dll!_wsplitpath_s"
                && reason.contains("overlapping"))
    );
    // Read faults, readonly participating outputs, and size/address overflow
    // fail before copying even the drive, without invoking an invalid handler.
    for args in [
        parts(SOURCE_W, [DRIVE, DIR, NAME, READ_ONLY], [3, 256, 256, 256]),
        parts(SOURCE_W, [DRIVE, DIR, NAME, u32::MAX - 1], [3, 256, 256, 2]),
        parts(0x12345678, addresses, [3, 256, 256, 256]),
    ] {
        let mut machine = fixture(cpu());
        let api = machine
            .resolve_import("msvcr90.dll", "_wsplitpath_s")
            .unwrap();
        let mut code = Asm::new(CODE);
        call(&mut code, api, &args);
        let result = execute(&mut machine, code);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert_eq!(read(&machine, DRIVE, 6), [0x5a; 6]);
        assert_eq!(read(&machine, DIR, 16), [0x5a; 16]);
    }
}

#[test]
fn secure_split_path_cdecl_aliases_preserve_units_dbcs_and_capacity_guards() {
    success(UnicornCpu::new(PARK_ADDR).unwrap());
    success(JitCpu::new(PARK_ADDR, false).unwrap());
}

#[test]
fn secure_split_path_native_failure_order_and_atomic_memory_guards() {
    failure(|| UnicornCpu::new(PARK_ADDR).unwrap());
    failure(|| JitCpu::new(PARK_ADDR, false).unwrap());
}
