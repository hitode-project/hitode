//! USER32 string traversal and case conversion, with ACP and DBCS boundaries.
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, win32};
use adv_cpu::backend::{Guest, GuestExt, MemFault, Prot};
use adv_machine::{Env, Outcome};
use adv_os::Codec;
use adv_os::nls::{CP_ACP, CP_OEMCP, unit_case};

use crate::{Win32, stdcall};

fn next_ansi(cpu: &dyn Guest, codec: Codec, address: u32) -> Result<u32, MemFault> {
    let byte = cpu.read_u8(address)?;
    if byte == 0 {
        return Ok(address);
    }
    let next = address.wrapping_add(1);
    // A dangling lead byte advances to the terminator, never past it. Only
    // DBCS leads require reading a second byte (including at a page boundary).
    if codec.is_lead_byte(CP_ACP, byte) && cpu.read_u8(next)? != 0 {
        Ok(next.wrapping_add(1))
    } else {
        Ok(next)
    }
}

#[win32(user32, name = "CharNextA", level = implement)]
fn char_next(env: &mut Env<'_>, text: AnsiStr) -> Result<AnsiStr, MemFault> {
    next_ansi(env.cpu, env.ext.get::<Win32>().codec, text.0.addr())
        .map(|address| AnsiStr(GuestPtr::new(address)))
}

#[win32(user32, name = "CharNextW", level = implement)]
fn char_next_w(env: &mut Env<'_>, text: WideStr) -> Result<WideStr, MemFault> {
    let mut next = text.0;
    if next.read(env.cpu)? == 0 {
        return Ok(text);
    }
    next = next.offset(1)?;
    while adv_os::nls::char_next_skips(next.read(env.cpu)?) {
        next = next.offset(1)?;
    }
    Ok(WideStr(next))
}

#[win32(user32, name = "CharUpperW", level = implement)]
fn char_upper_w(env: &mut Env<'_>, text: WideStr) -> Outcome {
    wide_case(env, text, true)
}

#[win32(user32, name = "CharLowerW", level = implement)]
fn char_lower_w(env: &mut Env<'_>, text: WideStr) -> Outcome {
    wide_case(env, text, false)
}

fn wide_case(env: &mut Env<'_>, text: WideStr, upper: bool) -> Outcome {
    let address = text.0.addr();
    if address >> 16 == 0 {
        return Outcome::Return(u32::from(unit_case(address as u16, upper)));
    }
    let Ok(units) = text.read(env.cpu, 16 * 1024 * 1024) else {
        return Outcome::Fail("CharUpper/LowerW: invalid string".into());
    };
    let bytes: Vec<_> = units
        .into_iter()
        .flat_map(|u| unit_case(u, upper).to_le_bytes())
        .collect();
    match env.cpu.write(address, &bytes) {
        Ok(()) => Outcome::Return(address),
        Err(error) => Outcome::Fail(error.to_string()),
    }
}

#[win32(user32, name = "OemToCharW", level = implement)]
fn oem_to_char_w(
    env: &mut Env<'_>,
    source: AnsiStr,
    destination: WideStr,
) -> Result<i32, MemFault> {
    let input = source.read(env.cpu, 16 * 1024 * 1024)?;
    let mut units = env.ext.get::<Win32>().codec.to_utf16(CP_OEMCP, &input);
    units.push(0);
    let bytes: Vec<_> = units.into_iter().flat_map(u16::to_le_bytes).collect();
    adv_abi::guest::check_range(
        env.cpu,
        destination.0.addr(),
        bytes.len() as u32,
        Prot::WRITE,
    )?;
    env.cpu.write(destination.0.addr(), &bytes)?;
    Ok(1)
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

// In the packed form only the low byte is converted. The remaining low-word
// bits survive, and a DBCS lead byte is not followed outside that byte.
fn packed_case(character: u32, codec: Codec, upper: bool) -> u32 {
    let mut byte = [character as u8];
    ansi_case(&mut byte, codec, upper);
    (character & !0xff) | u32::from(byte[0])
}

#[win32(user32, name = "OemToCharA", level = implement)]
fn oem_to_char(env: &mut Env<'_>, source: AnsiStr, destination: AnsiStr) -> Result<i32, MemFault> {
    // Read before writing: the ANSI API explicitly permits identical pointers.
    let input = source.read(env.cpu, 16 * 1024 * 1024)?;
    let codec = env.ext.get::<Win32>().codec;
    let mut output = if codec.resolve(CP_OEMCP) == codec.resolve(CP_ACP) {
        // Windows copies bytes when the two pages coincide, including an
        // isolated DBCS lead byte. A Unicode round trip would corrupt those.
        input
    } else {
        codec
            .from_utf16(CP_ACP, &codec.to_utf16(CP_OEMCP, &input))
            .0
    };
    output.push(0);
    adv_abi::guest::check_range(
        env.cpu,
        destination.0.addr(),
        output.len() as u32,
        Prot::WRITE,
    )?;
    env.cpu.write(destination.0.addr(), &output)?;
    Ok(1)
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
    reg.register(char_next_ABI);
    reg.register(char_upper_ABI);
    reg.register(char_lower_ABI);
    reg.register(oem_to_char_ABI);
    reg.register(char_next_w_ABI);
    reg.register(char_upper_w_ABI);
    reg.register(char_lower_w_ABI);
    reg.register(oem_to_char_w_ABI);
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
        fn CharNextExA(codepage: u16, text: *const u8, flags: u32) -> *mut u8;
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
    fn ansi_next_matches_windows_dbcs_and_terminator_boundaries() {
        use adv_cpu::{JitCpu, backend::Guest};

        let mut cpu = JitCpu::new(0x400000, false).unwrap();
        cpu.map(0x410000, 4096, super::Prot::RW).unwrap();
        for codepage in [932, 936, 1252] {
            let codec = super::Codec::new(codepage);
            for first in 0..=u8::MAX {
                for second in [0, 0x20, 0x40, 0x81, 0xff] {
                    let bytes = [first, second, 0];
                    cpu.write(0x410000, &bytes).unwrap();
                    // SAFETY: the input is a readable, null-terminated buffer.
                    let expected = unsafe {
                        CharNextExA(codepage as u16, bytes.as_ptr(), 0).offset_from(bytes.as_ptr())
                    } as u32;
                    assert_eq!(
                        super::next_ansi(&cpu, codec, 0x410000).unwrap() - 0x410000,
                        expected,
                        "CP={codepage} bytes={bytes:02x?}"
                    );
                }
            }
        }
        let end = 0x410fff;
        cpu.write(end, b"a").unwrap();
        assert_eq!(
            super::next_ansi(&cpu, super::Codec::new(932), end).unwrap(),
            end + 1
        );
        cpu.write(end, &[0]).unwrap();
        assert_eq!(
            super::next_ansi(&cpu, super::Codec::new(932), end).unwrap(),
            end
        );
        cpu.write(end, &[0x82]).unwrap();
        assert!(super::next_ansi(&cpu, super::Codec::new(932), end).is_err());
        assert!(super::next_ansi(&cpu, super::Codec::new(932), 0).is_err());
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
