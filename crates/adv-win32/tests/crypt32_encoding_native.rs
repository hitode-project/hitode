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
}
#[test]
// spec: PERS-CRYPTO-7
fn byte_encodings_match_native_crypt32() {
    let mut failures = Vec::new();
    for length in [0usize, 1, 2, 3, 8, 15, 16, 17, 47, 48, 49, 64, 255, 256] {
        let input = (0..length).map(|value| value as u8).collect::<Vec<_>>();
        for format in [0, 1, 3, 4, 5, 9, 10, 11, 12] {
            for ending in [0, 0x40000000, 0x80000000] {
                let flags = format | ending;
                let mut size = 0;
                assert_ne!(
                    unsafe {
                        CryptBinaryToStringA(
                            input.as_ptr(),
                            length as u32,
                            flags,
                            std::ptr::null_mut(),
                            &mut size,
                        )
                    },
                    0
                );
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
                let encoded = encoding::encode(&input, flags).unwrap();
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
// spec: PERS-CRYPTO-7
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
        let terminated = input.bytes().chain([0]).collect::<Vec<_>>();
        for format in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
            for strict in [0, 0x20000000] {
                let flags = format | strict;
                let mut size = 4096;
                let mut skip = u32::MAX;
                let mut detected = u32::MAX;
                let mut output = vec![0xcd; size as usize];
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
                let error = unsafe { GetLastError() };
                output.truncate(size as usize);
                let native = if result == 0 {
                    Err(error)
                } else {
                    Ok((output, skip, detected))
                };
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
