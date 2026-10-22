use super::*;
use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
#[test]
// spec: PERS-CRYPTO-7
fn encodings_match_rfc_vectors_and_preserve_header_detection() {
    for (bytes, base64) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(
            encoding::encode(bytes.as_bytes(), 0x40000001).unwrap(),
            base64.as_bytes()
        );
        let text = base64.encode_utf16().collect::<Vec<_>>();
        assert_eq!(
            encoding::decode(&text, &[], 1).unwrap(),
            (bytes.as_bytes().to_vec(), 0, 1)
        );
    }
    for length in [0, 1, 2, 3, 8, 15, 16, 17, 47, 48, 49, 64, 255, 256] {
        let bytes = (0..length).map(|value| value as u8).collect::<Vec<_>>();
        for kind in [0, 1, 3, 4, 5, 9, 10, 11, 12] {
            for line_mode in [0, 0x80000000, 0x40000000] {
                let output = encoding::encode(&bytes, kind | line_mode).unwrap();
                let text = output.iter().copied().map(u16::from).collect::<Vec<_>>();
                if length == 0 && matches!(kind, 5 | 10 | 11) {
                    assert_eq!(encoding::decode(&text, &output, kind), Err(13));
                    continue;
                }
                assert_eq!(
                    encoding::decode(&text, &output, kind).unwrap().0,
                    bytes,
                    "{length} {kind} {line_mode:x}"
                );
            }
        }
    }
    let text = "ignored\r\n-----BEGIN PUBLIC KEY-----\r\nZm9v\r\n-----END DIFFERENT-----\r\n"
        .encode_utf16()
        .collect::<Vec<_>>();
    assert_eq!(
        encoding::decode(&text, &[], 6).unwrap(),
        (b"foo".to_vec(), 9, 0)
    );
    assert_eq!(encoding::encode(b"foo", 3).unwrap(), b"-----BEGIN NEW CERTIFICATE REQUEST-----\r\nZm9v\r\n-----END NEW CERTIFICATE REQUEST-----\r\n");
    assert_eq!(
        encoding::encode(b"foo", 9 | 0x80000000).unwrap(),
        b"-----BEGIN X509 CRL-----\nZm9v\n-----END X509 CRL-----\n"
    );
    assert_eq!(
        encoding::encode(&[0, 0xab, 0xff], 4).unwrap(),
        b"00 ab ff\r\n"
    );
    assert_eq!(
        encoding::encode(&[0, 0xab, 0xff], 12).unwrap(),
        b"00abff\r\n"
    );
    for text in ["?", "=", "===", "a?aa", "Zm9v====", "Zm9v=tail"] {
        let units = text.encode_utf16().collect::<Vec<_>>();
        assert_eq!(encoding::decode(&units, &[], 0x20000001), Err(13), "{text}");
    }
    for (text, byte) in [("Z", 100), ("0", 212)] {
        assert_eq!(
            encoding::decode(&text.encode_utf16().collect::<Vec<_>>(), &[], 1)
                .unwrap()
                .0,
            [byte]
        );
    }
    assert_eq!(
        encoding::encode(&[0, 1, 2], 10).unwrap(),
        b"0000\t00 01 02\r\n"
    );
    assert_eq!(
        encoding::encode(&(0..17).collect::<Vec<_>>(), 0x40000004).unwrap(),
        b"00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f 10"
    );
    let text = "Zm9v=ignored".encode_utf16().collect::<Vec<_>>();
    assert_eq!(encoding::decode(&text, &[], 1).unwrap().0, b"foo");
    for text in ["0", "0x00", "01?2", "aabbz"] {
        assert_eq!(
            encoding::decode(&text.encode_utf16().collect::<Vec<_>>(), &[], 4),
            Err(13)
        );
    }
    assert_eq!(encoding::encode(b"x", 6), Err(87));
    assert_eq!(encoding::decode(&[], &[], 0x40000001), Err(87));
    assert_eq!(
        encoding::decode(&[0xff, 0], &[0xff, 0], 7).unwrap(),
        (vec![0xff, 0], 0, 2)
    );
}
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const INPUT: u32 = DATA + 0x2000;
const TEXT: u32 = DATA + 0x2400;
const OUTPUT: u32 = DATA + 0x2800;
const SIZE: u32 = DATA + 0x3000;
const SKIP: u32 = SIZE + 4;
const FORMAT: u32 = SIZE + 8;
const MARKER: u32 = 0x12345678;
fn guest<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(".", "fixture.exe", "target/crypt32-tests"),
    )
    .unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
    machine.cpu.write(INPUT, b"foobar").unwrap();
    machine.cpu.write(OUTPUT, &[0xcd; 512]).unwrap();
    machine.cpu.map(DATA + 0x4000, 4096, Prot::READ).unwrap();
    machine.cpu.write_u32(DATA + 0x4000, 0xdeadbeef).unwrap();
    let mut code = Asm::new(CODE);
    let mut rows = Vec::new();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    for (index, suffix) in ["A", "W"].into_iter().enumerate() {
        let mut call = |code: &mut Asm, name: &str, args: &[u32], expected: u32, error: u32| {
            code.push_imm(MARKER).call(set_error);
            for &arg in args.iter().rev() {
                code.push_imm(arg);
            }
            let target = machine
                .resolve_import(DLL, &format!("{name}{suffix}"))
                .unwrap();
            let row = DATA + rows.len() as u32 * 12;
            code.call(target)
                .mov_mem_eax(row)
                .call(get_error)
                .mov_mem_eax(row + 4)
                .mov_eax_mem(SIZE)
                .mov_mem_eax(row + 8);
            rows.push((expected, error));
        };
        code.mov_mem_imm(SIZE, 0);
        call(
            &mut code,
            "CryptBinaryToString",
            &[INPUT, 6, 1, 0, SIZE],
            1,
            MARKER,
        );
        code.mov_mem_imm(SIZE, 1);
        call(
            &mut code,
            "CryptBinaryToString",
            &[INPUT, 6, 1, OUTPUT, SIZE],
            0,
            234,
        );
        code.mov_mem_imm(SIZE, 11);
        call(
            &mut code,
            "CryptBinaryToString",
            &[INPUT, 6, 1, TEXT, SIZE],
            1,
            MARKER,
        );
        code.mov_mem_imm(SIZE, 0);
        call(
            &mut code,
            "CryptStringToBinary",
            &[TEXT, 10, 1, 0, SIZE, SKIP, FORMAT],
            1,
            MARKER,
        );
        code.mov_mem_imm(SIZE, 1);
        call(
            &mut code,
            "CryptStringToBinary",
            &[TEXT, 0, 1, OUTPUT, SIZE, SKIP, FORMAT],
            0,
            234,
        );
        code.mov_mem_imm(SIZE, 6);
        call(
            &mut code,
            "CryptStringToBinary",
            &[TEXT, 0, 1, OUTPUT + index as u32 * 64, SIZE, SKIP, FORMAT],
            1,
            MARKER,
        );
        call(
            &mut code,
            "CryptStringToBinary",
            &[TEXT, 8, 1, OUTPUT, SIZE, DATA + 0x4000, FORMAT],
            0,
            87,
        );
        code.mov_mem_imm(SIZE, 64);
        call(
            &mut code,
            "CryptBinaryToString",
            &[INPUT, 6, 1, DATA + 0x4000, SIZE],
            0,
            87,
        );
        code.mov_mem_imm(SIZE, 64);
        call(
            &mut code,
            "CryptBinaryToString",
            &[INPUT, 6, 6, OUTPUT, SIZE],
            0,
            87,
        );
        // Preserve the exact encoded text across the next A/W call.
        let saved = TEXT + 128 + index as u32 * 128;
        call(
            &mut code,
            "CryptBinaryToString",
            &[INPUT, 6, 0x40000001, saved, SIZE],
            1,
            MARKER,
        );
    }
    code.mov_mem_imm(DATA + 0x3f00, 0)
        .mov_eax_mem(DATA + 0x3f00)
        .ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("binary encodings", CODE, &[], 65536).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, (expected, error)) in rows.into_iter().enumerate() {
        let row = DATA + index as u32 * 12;
        assert_eq!(machine.cpu.read_u32(row).unwrap(), expected, "call {index}");
        assert_eq!(
            machine.cpu.read_u32(row + 4).unwrap(),
            error,
            "call {index} LastError"
        );
        if index % 10 < 6 {
            assert_eq!(
                machine.cpu.read_u32(row + 8).unwrap(),
                [11, 11, 10, 6, 6, 6][index % 10],
                "call {index} size"
            );
        }
    }
    let mut output = [0; 6];
    for index in 0..2 {
        machine.cpu.read(OUTPUT + index * 64, &mut output).unwrap();
        assert_eq!(&output, b"foobar");
    }
    machine.cpu.read(OUTPUT + 128, &mut output).unwrap();
    assert_eq!(output, [0xcd; 6]);
    assert_eq!(machine.cpu.read_u32(SKIP).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(FORMAT).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 0x4000).unwrap(), 0xdeadbeef);
    let mut narrow = [0; 9];
    machine.cpu.read(TEXT + 128, &mut narrow).unwrap();
    assert_eq!(&narrow, b"Zm9vYmFy\0");
    let mut wide = [0; 18];
    machine.cpu.read(TEXT + 256, &mut wide).unwrap();
    assert_eq!(
        wide.to_vec(),
        "Zm9vYmFy\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    );
    let diagnostics = machine.ext.get_or_default::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
    assert!(diagnostics.acceptance_eligible(RunMode::Strict));
}
#[test]
// spec: PERS-CRYPTO-7, PERS-CRYPTO-8
fn binary_conversion_sizes_and_guest_permissions_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        guest(cpu);
    }
    guest(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn raw_copies<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(".", "fixture.exe", "target/crypt32-raw-tests"),
    )
    .unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
    machine
        .cpu
        .write(INPUT, b"ABCDEFGH\0\0\0\0\0\0\0\0")
        .unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    for (wide, suffix) in [(false, "A"), (true, "W")] {
        for capacity in [0u32, 3, 16] {
            let target = machine
                .resolve_import(DLL, &format!("CryptStringToBinary{suffix}"))
                .unwrap();
            let row = DATA + expected.len() as u32 * 128;
            machine.cpu.write(row + 16, &[0xcd; 32]).unwrap();
            code.mov_mem_imm(SIZE, capacity)
                .push_imm(FORMAT)
                .push_imm(SKIP)
                .push_imm(SIZE)
                .push_imm(if capacity == 0 { 0 } else { row + 16 })
                .push_imm(2)
                .push_imm(if wide { 2 } else { 4 })
                .push_imm(INPUT)
                .call(target)
                .mov_mem_eax(row)
                .mov_eax_mem(SIZE)
                .mov_mem_eax(row + 4);
            expected.push((capacity, wide));
        }
    }
    code.mov_mem_imm(DATA + 0x3f00, 0)
        .mov_eax_mem(DATA + 0x3f00)
        .ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("raw copies", CODE, &[], 65536).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, (capacity, wide)) in expected.into_iter().enumerate() {
        let row = DATA + index as u32 * 128;
        assert_eq!(machine.cpu.read_u32(row).unwrap(), 1);
        assert_eq!(
            machine.cpu.read_u32(row + 4).unwrap(),
            if capacity != 0 {
                capacity
            } else if wide {
                2
            } else {
                4
            }
        );
        let mut bytes = [0; 32];
        machine.cpu.read(row + 16, &mut bytes).unwrap();
        let mut expected = [0xcd; 32];
        expected[..capacity as usize]
            .copy_from_slice(&b"ABCDEFGH\0\0\0\0\0\0\0\0"[..capacity as usize]);
        assert_eq!(bytes, expected);
    }
    assert_eq!(machine.cpu.read_u32(FORMAT).unwrap(), 2);
    assert_eq!(machine.cpu.read_u32(SKIP).unwrap(), 0);
}
#[test]
// spec: PERS-CRYPTO-9
fn raw_binary_uses_native_character_queries_and_checked_byte_copy_capacities() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        raw_copies(cpu);
    }
    raw_copies(UnicornCpu::new(PARK_ADDR).unwrap());
}
