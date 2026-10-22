#![cfg(windows)]
//! Compare authored byte sequences with the host crypt32 encoder. These checks
//! also run in the repository's advisory Windows differential workflow.
#[path = "../src/crypt32/encoding.rs"]
mod encoding;
#[link(name = "crypt32")]
unsafe extern "system" {
    fn CryptBinaryToStringA(
        input: *const u8,
        length: u32,
        flags: u32,
        output: *mut u8,
        size: *mut u32,
    ) -> i32;
    fn CryptStringToBinaryA(
        input: *const u8,
        length: u32,
        flags: u32,
        output: *mut u8,
        size: *mut u32,
        skip: *mut u32,
        detected: *mut u32,
    ) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
    fn SetLastError(error: u32);
}
#[test]
// spec: SYS-CRYPTO-7
fn byte_encodings_match_native_crypt32() {
    let mut failures = Vec::new();
    for length in [0usize, 1, 2, 3, 8, 15, 16, 17, 47, 48, 49, 64, 255, 256] {
        let backing = (0..length.max(1))
            .map(|value| value as u8)
            .collect::<Vec<_>>();
        let input = &backing[..length];
        for format in [0, 1, 3, 4, 5, 9, 10, 11, 12] {
            for ending in [0, 0x40000000, 0x80000000] {
                let flags = format | ending;
                let mut size = 0;
                let query = unsafe {
                    CryptBinaryToStringA(
                        input.as_ptr(),
                        length as u32,
                        flags,
                        std::ptr::null_mut(),
                        &mut size,
                    )
                };
                if query == 0 {
                    let error = unsafe { GetLastError() };
                    assert_eq!(length, 0, "flags={flags:x} error={error}");
                    assert_eq!(error, 87, "empty encoding flags={flags:x}");
                    continue;
                }
                let mut native = vec![0xcd; size as usize];
                assert_ne!(
                    unsafe {
                        CryptBinaryToStringA(
                            input.as_ptr(),
                            length as u32,
                            flags,
                            native.as_mut_ptr(),
                            &mut size,
                        )
                    },
                    0
                );
                native.truncate(size as usize);
                let encoded = encoding::encode(input, flags).unwrap();
                if encoded != native {
                    eprintln!(
                        "length={length} flags={flags:x} native={native:?} guest={encoded:?}"
                    );
                    failures.push((length, flags));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "native encoding mismatches: {failures:?}"
    );
}
#[test]
// spec: SYS-CRYPTO-7
fn decoding_and_detection_match_native_crypt32() {
    let mut failures = Vec::new();
    for input in [
        "",
        "Zg==",
        "Zm8=",
        "Zm9v",
        "Z g\r\n==",
        "Zg",
        "Z",
        "?",
        "=",
        "===",
        "====",
        "Zm9v=ignored",
        "Zm9v====",
        "Zm9v=tail",
        "Zm9v===",
        "Zm9v\\0tail",
        "00 ab ff",
        "00abff",
        "0",
        "0x00",
        "0000  41 42 43",
        "preamble\r\n-----BEGIN PUBLIC KEY-----\r\nZm9v\r\n-----END DIFFERENT-----\r\n",
    ] {
        let units = input.encode_utf16().collect::<Vec<_>>();
        // Some native binary-copy paths use the output count as the copy
        // length. Keep every possible source byte inside an authored zeroed
        // allocation, query the exact size, and never log beyond that extent.
        let mut terminated = vec![0u8; 4096];
        terminated[..input.len()].copy_from_slice(input.as_bytes());
        for format in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
            for strict in [0, 0x20000000] {
                let flags = format | strict;
                let mut size = 0;
                let mut skip = u32::MAX;
                let mut detected = u32::MAX;
                let query = unsafe {
                    CryptStringToBinaryA(
                        terminated.as_ptr(),
                        input.len() as u32,
                        flags,
                        std::ptr::null_mut(),
                        &mut size,
                        &mut skip,
                        &mut detected,
                    )
                };
                assert!(size <= terminated.len() as u32);
                let native = if query == 0 {
                    Err(unsafe { GetLastError() })
                } else {
                    let mut output = vec![0xcd; (size as usize).max(1)];
                    let result = unsafe {
                        CryptStringToBinaryA(
                            terminated.as_ptr(),
                            input.len() as u32,
                            flags,
                            output.as_mut_ptr(),
                            &mut size,
                            &mut skip,
                            &mut detected,
                        )
                    };
                    assert!(size as usize <= output.len());
                    if result == 0 {
                        Err(unsafe { GetLastError() })
                    } else {
                        output.truncate(size as usize);
                        Ok((output, skip, detected))
                    }
                };
                // URI Base64 is a later Windows extension outside the Win7
                // format set. Do not use that path as a Win7 reference.
                if native.as_ref().is_ok_and(|(_, _, format)| *format == 13) {
                    continue;
                }
                let guest = encoding::decode(&units, input.as_bytes(), flags);
                if native != guest {
                    eprintln!("input={input:?} flags={flags:x} native={native:?} guest={guest:?}");
                    failures.push((input, flags));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "native decoding mismatches: {failures:?}"
    );
}

#[link(name = "crypt32")]
unsafe extern "system" {
    fn CryptBinaryToStringW(
        input: *const u8,
        length: u32,
        flags: u32,
        output: *mut u16,
        size: *mut u32,
    ) -> i32;
    fn CryptStringToBinaryW(
        input: *const u16,
        length: u32,
        flags: u32,
        output: *mut u8,
        size: *mut u32,
        skip: *mut u32,
        detected: *mut u32,
    ) -> i32;
}
#[test]
// spec: SYS-CRYPTO-8, SYS-CRYPTO-9
fn raw_binary_copies_match_native_buffer_and_length_rules() {
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt, Prot, asm::Asm},
    };
    use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
    use adv_win32::{Win32, Win32Config};
    let mut source = [0u16; 32];
    source[..4].copy_from_slice(&[0x4241, 0x4443, 0x4645, 0x4847]);
    let source_bytes = source
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut failures = Vec::new();
    for wide in [false, true] {
        for encoding in [false, true] {
            for capacity in [0u32, 3, 16] {
                let mut native_output = [0xcd; 64];
                let mut native_size = capacity;
                let input_length = if encoding || !wide { 4 } else { 2 };
                unsafe { SetLastError(0x12345678) };
                let native_result = unsafe {
                    let destination = if capacity == 0 {
                        std::ptr::null_mut()
                    } else {
                        native_output.as_mut_ptr()
                    };
                    if encoding && wide {
                        CryptBinaryToStringW(
                            source.as_ptr().cast(),
                            input_length,
                            2,
                            destination.cast(),
                            &mut native_size,
                        )
                    } else if encoding {
                        CryptBinaryToStringA(
                            source.as_ptr().cast(),
                            input_length,
                            2,
                            destination,
                            &mut native_size,
                        )
                    } else if wide {
                        CryptStringToBinaryW(
                            source.as_ptr(),
                            input_length,
                            2,
                            destination,
                            &mut native_size,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    } else {
                        CryptStringToBinaryA(
                            source.as_ptr().cast(),
                            input_length,
                            2,
                            destination,
                            &mut native_size,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    }
                };
                let native_error = unsafe { GetLastError() };
                let mut registry = adv_abi::Registry::default();
                adv_win32::register(&mut registry);
                let cpu = JitCpu::new(PARK_ADDR, false).unwrap();
                let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
                Win32::install(
                    &mut machine,
                    Win32Config::new(".", "fixture.exe", "target/native-crypt32-binary"),
                )
                .unwrap();
                const CODE: u32 = 0x400000;
                const DATA: u32 = 0x410000;
                machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
                machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
                machine.cpu.write(DATA, &source_bytes).unwrap();
                machine.cpu.write(DATA + 128, &[0xcd; 64]).unwrap();
                machine.cpu.write_u32(DATA + 256, capacity).unwrap();
                let name = format!(
                    "Crypt{}{}",
                    if encoding {
                        "BinaryToString"
                    } else {
                        "StringToBinary"
                    },
                    if wide { "W" } else { "A" }
                );
                let api = machine.resolve_import("crypt32.dll", &name).unwrap();
                let set_error = machine
                    .resolve_import("kernel32.dll", "SetLastError")
                    .unwrap();
                let get_error = machine
                    .resolve_import("kernel32.dll", "GetLastError")
                    .unwrap();
                let mut args = vec![
                    DATA,
                    input_length,
                    2,
                    if capacity == 0 { 0 } else { DATA + 128 },
                    DATA + 256,
                ];
                if !encoding {
                    args.extend([0, 0]);
                }
                let mut code = Asm::new(CODE);
                code.push_imm(0x12345678).call(set_error);
                for arg in args.into_iter().rev() {
                    code.push_imm(arg);
                }
                code.call(api)
                    .mov_mem_eax(DATA + 260)
                    .call(get_error)
                    .mov_mem_eax(DATA + 264)
                    .mov_mem_imm(DATA + 268, 0)
                    .mov_eax_mem(DATA + 268)
                    .ret();
                machine.cpu.write(CODE, &code.bytes).unwrap();
                machine.spawn("binary control", CODE, &[], 65536).unwrap();
                assert!(matches!(
                    machine.run_virtual(1_000_000),
                    RunResult::Exited(0)
                ));
                let mut guest_output = [0; 64];
                machine.cpu.read(DATA + 128, &mut guest_output).unwrap();
                let guest_size = machine.cpu.read_u32(DATA + 256).unwrap();
                let guest_result = machine.cpu.read_u32(DATA + 260).unwrap();
                let guest_error = machine.cpu.read_u32(DATA + 264).unwrap();
                if guest_result != u32::from(native_result != 0)
                    || (native_result == 0 && guest_error != native_error)
                    || native_size != guest_size
                    || native_output != guest_output
                {
                    eprintln!(
                        "{name} capacity={capacity} native-size={native_size} guest-size={guest_size} native-result={native_result} guest-result={guest_result} native-error={native_error} guest-error={guest_error} native={:?} guest={:?}",
                        &native_output[..20],
                        &guest_output[..20]
                    );
                    failures.push((name, capacity));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "native binary mismatches: {failures:?}"
    );
}
