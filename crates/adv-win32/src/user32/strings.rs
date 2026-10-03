//! In-place USER32 case conversion, with ACP and DBCS character boundaries.
use adv_abi::{AnsiStr, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
use adv_os::Codec;
use adv_os::nls::unit_case;

use crate::{Win32, stdcall};

fn ansi_case(bytes: &mut [u8], codec: Codec, upper: bool) {
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte < 0x80 {
            bytes[i] = if upper {
                byte.to_ascii_uppercase()
            } else {
                byte.to_ascii_lowercase()
            };
            i += 1;
            continue;
        }
        let n = if codec.is_lead_byte(0, byte) { 2 } else { 1 };
        if i + n > bytes.len() {
            break; // Do not read a DBCS trail byte outside the requested buffer.
        }
        let original = &bytes[i..i + n];
        let units = codec.to_utf16(0, original);
        if units.len() == 1 && units[0] != 0xfffd {
            let mapped = unit_case(units[0], upper);
            if mapped != units[0] {
                let (replacement, lossy) = codec.from_utf16(0, &[mapped]);
                if !lossy && replacement.len() == n {
                    bytes[i..i + n].copy_from_slice(&replacement);
                }
            }
        }
        i += n;
    }
}

// In the packed form only the low byte is converted. The remaining low-word
// bits survive, and a DBCS lead byte is not followed outside that byte.
fn packed_case(character: u32, codec: Codec, upper: bool) -> u32 {
    let mut byte = [character as u8];
    ansi_case(&mut byte, codec, upper);
    (character & !0xff) | u32::from(byte[0])
}

#[win32(user32, name = "CharUpperA", level = implement)]
fn char_upper(env: &mut Env<'_>, text: AnsiStr) -> Outcome {
    string_case(env, text, true)
}

#[win32(user32, name = "CharLowerA", level = implement)]
fn char_lower(env: &mut Env<'_>, text: AnsiStr) -> Outcome {
    string_case(env, text, false)
}

fn string_case(env: &mut Env<'_>, text: AnsiStr, upper: bool) -> Outcome {
    let address = text.0.addr();
    let codec = env.ext.get::<Win32>().codec;
    if address >> 16 == 0 {
        return Outcome::Return(packed_case(address, codec, upper));
    }
    let Ok(mut bytes) = env.cpu.read_cstr(address, 16 * 1024 * 1024) else {
        return Outcome::Fail("CharUpper/Lower: invalid string".into());
    };
    ansi_case(&mut bytes, codec, upper);
    if let Err(error) = env.cpu.write(address, &bytes) {
        return Outcome::Fail(error.to_string());
    }
    Outcome::Return(address)
}

fn convert(env: &mut Env<'_>, wide: bool, upper: bool) -> Outcome {
    let (ptr, count) = (env.arg(0), env.arg(1));
    if count == 0 {
        return Outcome::Return(0);
    }
    let Some(bytes) = count.checked_mul(if wide { 2 } else { 1 }) else {
        return Outcome::Return(0);
    };
    if ptr.checked_add(bytes).is_none() || !env.cpu.accessible(ptr, bytes, Prot::RW) {
        return Outcome::Return(0);
    }
    let codec = env.ext.get::<Win32>().codec;
    let mut scratch = [0u8; 4096];
    let mut offset = 0;
    while offset < bytes {
        let mut n = (bytes - offset).min(scratch.len() as u32) as usize;
        if let Err(error) = env.cpu.read(ptr + offset, &mut scratch[..n]) {
            return Outcome::Fail(error.to_string());
        }
        if wide {
            for pair in scratch[..n].as_chunks_mut::<2>().0 {
                *pair = unit_case(u16::from_le_bytes(*pair), upper).to_le_bytes();
            }
        } else {
            // Count character boundaries before splitting; a DBCS trail byte can
            // itself have a lead-byte value or be ASCII a-z.
            if offset + (n as u32) < bytes {
                let mut boundary = 0;
                while boundary < n {
                    let width = if codec.is_lead_byte(0, scratch[boundary]) {
                        2
                    } else {
                        1
                    };
                    if boundary + width > n {
                        n = boundary;
                        break;
                    }
                    boundary += width;
                }
            }
            ansi_case(&mut scratch[..n], codec, upper);
        }
        if let Err(error) = env.cpu.write(ptr + offset, &scratch[..n]) {
            return Outcome::Fail(error.to_string());
        }
        offset += n as u32;
    }
    Outcome::Return(count)
}

pub(super) fn register(reg: &mut Registry) {
    reg.register(char_upper_ABI);
    reg.register(char_lower_ABI);
    for (name, handler) in [
        (
            "CharUpperBuffA",
            (|e: &mut Env<'_>| convert(e, false, true)) as adv_machine::Handler,
        ),
        (
            "CharLowerBuffA",
            (|e: &mut Env<'_>| convert(e, false, false)) as adv_machine::Handler,
        ),
        (
            "CharUpperBuffW",
            (|e: &mut Env<'_>| convert(e, true, true)) as adv_machine::Handler,
        ),
        (
            "CharLowerBuffW",
            (|e: &mut Env<'_>| convert(e, true, false)) as adv_machine::Handler,
        ),
    ] {
        stdcall(reg, super::USER32, name, 2, handler);
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn CharUpperA(text: *mut u8) -> *mut u8;
        fn CharLowerA(text: *mut u8) -> *mut u8;
        fn CharUpperBuffW(text: *mut u16, len: u32) -> u32;
        fn CharLowerBuffW(text: *mut u16, len: u32) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetACP() -> u32;
    }

    #[test]
    fn packed_ansi_characters_match_windows_for_entire_low_word() {
        // SAFETY: GetACP has no parameters or memory access preconditions.
        let codec = super::Codec::new(unsafe { GetACP() });
        for upper in [false, true] {
            for character in 0..=u16::MAX {
                let input = usize::from(character) as *mut u8;
                // SAFETY: a zero high word selects Windows' packed-character
                // form; no pointer is dereferenced for these inputs.
                let expected = unsafe {
                    if upper {
                        CharUpperA(input)
                    } else {
                        CharLowerA(input)
                    }
                } as usize as u32;
                assert_eq!(
                    super::packed_case(u32::from(character), codec, upper),
                    expected,
                    "ACP={} upper={upper} input={character:#06x}",
                    codec.acp
                );
            }
        }
    }

    #[test]
    fn bmp_mapping_matches_windows() {
        for upper in [false, true] {
            let mut reference: Vec<u16> = (0..=u16::MAX).collect();
            // SAFETY: the vector contains exactly len writable initialized units.
            unsafe {
                if upper {
                    CharUpperBuffW(reference.as_mut_ptr(), reference.len() as u32);
                } else {
                    CharLowerBuffW(reference.as_mut_ptr(), reference.len() as u32);
                }
            }
            let differences: Vec<_> = reference
                .iter()
                .enumerate()
                .filter_map(|(unit, &expected)| {
                    let actual = super::unit_case(unit as u16, upper);
                    (actual != expected)
                        .then_some(format!("{unit:04x}:{actual:04x}->{expected:04x}"))
                })
                .collect();
            assert!(
                differences.is_empty(),
                "upper={upper} {} differences: {:?}",
                differences.len(),
                differences
            );
        }
    }
}
