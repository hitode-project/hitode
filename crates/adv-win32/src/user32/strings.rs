//! Length-bounded, in-place USER32 case conversion. Counts include embedded NULs.
use adv_machine::{Env, Outcome, Registry};
use pal_cpu::Prot;

use crate::{Codec, Win32, stdcall};

#[path = "case_table.rs"]
mod case_table;

pub(crate) fn unit_case(unit: u16, upper: bool) -> u16 {
    if unit < 0x80 {
        return if upper {
            (unit as u8).to_ascii_uppercase()
        } else {
            (unit as u8).to_ascii_lowercase()
        } as u16;
    }
    // Pin USER32's legacy invariant mapping instead of inheriting Rust's full
    // Unicode expansion rules or changing behavior with the Rust toolchain.
    let table = if upper {
        case_table::UPPER
    } else {
        case_table::LOWER
    };
    let i = table.partition_point(|&(_, last, _, _)| last < unit);
    match table.get(i) {
        Some(&(first, _, stride, delta)) if unit >= first && (unit - first) % stride == 0 => {
            (i32::from(unit) + delta) as u16
        }
        _ => unit,
    }
}

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
            for pair in scratch[..n].chunks_exact_mut(2) {
                let mapped = unit_case(u16::from_le_bytes([pair[0], pair[1]]), upper);
                pair.copy_from_slice(&mapped.to_le_bytes());
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
        fn CharUpperBuffW(text: *mut u16, len: u32) -> u32;
        fn CharLowerBuffW(text: *mut u16, len: u32) -> u32;
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
