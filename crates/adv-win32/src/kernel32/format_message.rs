//! Bounded FormatMessage fronts over guest strings, the guest ACP and local heaps.
//! System messages currently cover common Win32 errors in neutral/US English.
//! Module message tables, other languages, finite wrapping widths and complex
//! insertion formats fail explicitly instead of inventing successful output.
//! Unsupported features report ERROR_NOT_SUPPORTED; unavailable system IDs and
//! languages report ERROR_MR_MID_NOT_FOUND and ERROR_RESOURCE_LANG_NOT_FOUND.
//! API contract: https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-formatmessagea
use crate::{Win32, util::set_last_error};
use adv_abi::{AnsiStr, Env, GuestPtr, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_os::{
    Heaps,
    global::{Block, Globals},
};

#[cfg(test)]
mod tests;

const ALLOCATE_BUFFER: u32 = 0x100;
const IGNORE_INSERTS: u32 = 0x200;
const FROM_STRING: u32 = 0x400;
const FROM_HMODULE: u32 = 0x800;
const FROM_SYSTEM: u32 = 0x1000;
const ARGUMENT_ARRAY: u32 = 0x2000;
const MAX_BYTES: usize = 128 * 1024;

pub(super) fn register(registry: &mut Registry) {
    registry.register(format_a_ABI);
    registry.register(format_w_ABI);
}

#[win32(kernel32, name="FormatMessageA", level=implement)]
fn format_a(
    env: &mut Env<'_>,
    flags: u32,
    source: GuestPtr<()>,
    message: u32,
    language: u32,
    buffer: AnsiStr,
    size: u32,
    arguments: GuestPtr<()>,
) -> u32 {
    format(
        env,
        flags,
        source.addr(),
        message,
        language,
        buffer.0.addr(),
        size,
        arguments.addr(),
        false,
    )
}
#[win32(kernel32, name="FormatMessageW", level=implement)]
fn format_w(
    env: &mut Env<'_>,
    flags: u32,
    source: GuestPtr<()>,
    message: u32,
    language: u32,
    buffer: WideStr,
    size: u32,
    arguments: GuestPtr<()>,
) -> u32 {
    format(
        env,
        flags,
        source.addr(),
        message,
        language,
        buffer.0.addr(),
        size,
        arguments.addr(),
        true,
    )
}

enum Failure {
    Error(u32),
    Unavailable(&'static str, u32),
}
fn failed(env: &mut Env<'_>, wide: bool, failure: Failure) -> u32 {
    let error = match failure {
        Failure::Error(error) => error,
        Failure::Unavailable(reason, error) => {
            adv_abi::warn_unavailable(
                env,
                if wide {
                    "kernel32.dll!FormatMessageW"
                } else {
                    "kernel32.dll!FormatMessageA"
                },
                reason,
            );
            error
        }
    };
    set_last_error(env, error);
    0
}

fn read_text(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Vec<u16>, Failure> {
    let limit = MAX_BYTES / if wide { 2 } else { 1 };
    let mut units = Vec::new();
    for index in 0..=limit {
        let offset = index as u32 * if wide { 2 } else { 1 };
        let at = pointer.checked_add(offset).ok_or(Failure::Error(998))?;
        let unit = if wide {
            GuestPtr::<u16>::new(at)
                .read(env.cpu)
                .map_err(|_| Failure::Error(998))?
        } else {
            u16::from(
                GuestPtr::<u8>::new(at)
                    .read(env.cpu)
                    .map_err(|_| Failure::Error(998))?,
            )
        };
        if unit == 0 {
            return Ok(if wide {
                units
            } else {
                let bytes: Vec<_> = units.into_iter().map(|u| u as u8).collect();
                env.ext.get::<Win32>().codec.to_utf16(0, &bytes)
            });
        }
        if index == limit {
            return Err(Failure::Error(234));
        }
        units.push(unit);
    }
    unreachable!()
}

fn argument(
    env: &Env<'_>,
    arguments: u32,
    array: bool,
    index: u32,
    spec: &[u16],
    wide: bool,
) -> Result<Vec<u16>, Failure> {
    if arguments == 0 {
        return Err(Failure::Error(87));
    }
    let address = if array {
        arguments
    } else {
        GuestPtr::<u32>::new(arguments)
            .read(env.cpu)
            .map_err(|_| Failure::Error(998))?
    };
    let at = address
        .checked_add((index - 1) * 4)
        .ok_or(Failure::Error(998))?;
    let value = GuestPtr::<u32>::new(at)
        .read(env.cpu)
        .map_err(|_| Failure::Error(998))?;
    match spec {
        [] | [115] => read_text(env, value, wide),
        [100] | [105] => Ok((value as i32).to_string().encode_utf16().collect()),
        [117] => Ok(value.to_string().encode_utf16().collect()),
        [120] => Ok(format!("{value:x}").encode_utf16().collect()),
        [88] => Ok(format!("{value:X}").encode_utf16().collect()),
        _ => Err(Failure::Unavailable(
            "inserts support default/!s! strings and !d!/!i!/!u!/!x!/!X! 32-bit integers; width, precision, alternate encoding and 64-bit formats are unavailable",
            50,
        )),
    }
}

fn render(
    mut insert: impl FnMut(u32, &[u16]) -> Result<Vec<u16>, Failure>,
    source: &[u16],
    ignore: bool,
    single_line: bool,
) -> Result<Vec<u16>, Failure> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < source.len() {
        let unit = source[index];
        index += 1;
        if matches!(unit, 10 | 13) {
            if unit == 13 && source.get(index) == Some(&10) {
                index += 1;
            }
            if single_line {
                output.push(32);
            } else {
                output.extend_from_slice(&[13, 10]);
            }
        } else if unit != 37 {
            output.push(unit);
        } else {
            let escaped = *source.get(index).ok_or(Failure::Error(87))?;
            index += 1;
            match escaped {
                48 => break,
                110 => output.extend_from_slice(&[13, 10]),
                114 => output.push(13),
                116 => output.push(9),
                49..=57 => {
                    let start = index - 2;
                    let mut number = u32::from(escaped - 48);
                    if let Some(&next @ 48..=57) = source.get(index) {
                        number = number * 10 + u32::from(next - 48);
                        index += 1;
                    }
                    let mut spec = &[][..];
                    if source.get(index) == Some(&33) {
                        let spec_start = index + 1;
                        let length = source[spec_start..]
                            .iter()
                            .position(|&u| u == 33)
                            .ok_or(Failure::Error(87))?;
                        spec = &source[spec_start..spec_start + length];
                        index = spec_start + length + 1;
                    }
                    if ignore {
                        // Native IGNORE_INSERTS drops the redundant !s! format.
                        if spec == [115] {
                            let digits_end = start + 2 + usize::from(number >= 10);
                            output.extend_from_slice(&source[start..digits_end]);
                        } else {
                            output.extend_from_slice(&source[start..index]);
                        }
                    } else {
                        output.extend(insert(number, spec)?);
                    }
                }
                _ => {
                    if ignore {
                        output.push(37);
                    }
                    output.push(escaped);
                }
            }
        }
        if output.len() > MAX_BYTES {
            return Err(Failure::Error(234));
        }
    }
    if output.is_empty() {
        Err(Failure::Error(235))
    } else {
        Ok(output)
    }
}

fn system_message(message: u32) -> Option<&'static str> {
    Some(match message {
        0 => "The operation completed successfully.\r\n",
        2 => "The system cannot find the file specified.\r\n",
        3 => "The system cannot find the path specified.\r\n",
        5 => "Access is denied.\r\n",
        6 => "The handle is invalid.\r\n",
        8 => "Not enough memory resources are available to process this command.\r\n",
        18 => "There are no more files.\r\n",
        32 => "The process cannot access the file because it is being used by another process.\r\n",
        33 => {
            "The process cannot access the file because another process has locked a portion of the file.\r\n"
        }
        50 => "The request is not supported.\r\n",
        80 => "The file exists.\r\n",
        87 => "The parameter is incorrect.\r\n",
        109 => "The pipe has been ended.\r\n",
        112 => "There is not enough space on the disk.\r\n",
        120 => "This function is not supported on this system.\r\n",
        122 => "The data area passed to a system call is too small.\r\n",
        123 => "The filename, directory name, or volume label syntax is incorrect.\r\n",
        126 => "The specified module could not be found.\r\n",
        127 => "The specified procedure could not be found.\r\n",
        183 => "Cannot create a file when that file already exists.\r\n",
        206 => "The filename or extension is too long.\r\n",
        234 => "More data is available.\r\n",
        258 => "The wait operation timed out.\r\n",
        317 => {
            "The system cannot find message text for message number 0x%1 in the message file for %2.\r\n"
        }
        995 => {
            "The I/O operation has been aborted because of either a thread exit or an application request.\r\n"
        }
        998 => "Invalid access to memory location.\r\n",
        1113 => {
            "No mapping for the Unicode character exists in the target multi-byte code page.\r\n"
        }
        1314 => "A required privilege is not held by the client.\r\n",
        1813 => "The specified resource type cannot be found in the image file.\r\n",
        1814 => "The specified resource name cannot be found in the image file.\r\n",
        1815 => "The specified resource language ID cannot be found in the image file.\r\n",
        _ => return None,
    })
}

fn allocate(env: &mut Env<'_>, bytes: &[u8], size: u32) -> Result<u32, Failure> {
    let size = size
        .max(bytes.len() as u32)
        .checked_add(7)
        .map(|n| n & !7)
        .ok_or(Failure::Error(8))?;
    if !env.sys.objects.can_insert() {
        return Err(Failure::Error(8));
    }
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    let pointer = heaps
        .alloc(env.cpu, &mut env.sys.vm, 0, size, false)
        .ok_or(Failure::Error(8))?;
    heaps.account_small(env.cpu, pointer);
    let block = Block {
        allocation: pointer,
        handle: 0,
        pointer,
        size,
        movable: false,
        locks: 0,
        attributes: 0,
        clipboard: false,
    };
    if env.cpu.write(pointer, bytes).is_err()
        || globals.insert(&mut env.sys.objects, block).is_none()
    {
        heaps.free(env.cpu, &mut env.sys.vm, 0, pointer);
        return Err(Failure::Error(8));
    }
    Ok(pointer)
}

#[expect(
    clippy::too_many_arguments,
    reason = "shared seven-parameter A/W front"
)]
fn format(
    env: &mut Env<'_>,
    flags: u32,
    source: u32,
    message: u32,
    language: u32,
    buffer: u32,
    size: u32,
    arguments: u32,
    wide: bool,
) -> u32 {
    let result =
        (|| {
            if flags & !0x3fff != 0
                || flags & (FROM_STRING | FROM_HMODULE | FROM_SYSTEM) == 0
                || flags & FROM_STRING != 0 && flags & (FROM_HMODULE | FROM_SYSTEM) != 0
            {
                return Err(Failure::Error(87));
            }
            if flags & 0xff != 0 && flags & 0xff != 0xff {
                return Err(Failure::Unavailable(
                    "finite maximum line widths are unavailable (supported: 0 and MAX_WIDTH_MASK)",
                    50,
                ));
            }
            if flags & FROM_HMODULE != 0 {
                return Err(Failure::Unavailable(
                    "module message-table resources and module/system fallback are unavailable",
                    50,
                ));
            }
            if !wide && !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
                return Err(Failure::Unavailable(
                    "ANSI formatting requires guest ACP 932/936/1252/65001",
                    50,
                ));
            }
            let text =
                if flags & FROM_STRING != 0 {
                    if source == 0 {
                        return Err(Failure::Error(87));
                    }
                    read_text(env, source, wide)?
                } else {
                    if !matches!(language, 0 | 0x409) {
                        return Err(Failure::Unavailable(
                            "system messages currently support neutral/US English LANGID only",
                            1815,
                        ));
                    }
                    system_message(message).ok_or(Failure::Unavailable(
                "message ID is absent from the supported common Win32 system-message table", 317))?
                .encode_utf16().collect()
                };
            let output = render(
                |index, spec| {
                    argument(
                        env,
                        arguments,
                        flags & ARGUMENT_ARRAY != 0,
                        index,
                        spec,
                        wide,
                    )
                },
                &text,
                flags & IGNORE_INSERTS != 0,
                flags & 0xff == 0xff,
            )?;
            let (mut bytes, count) = if wide {
                (
                    output
                        .iter()
                        .flat_map(|u| u.to_le_bytes())
                        .collect::<Vec<_>>(),
                    output.len(),
                )
            } else {
                let bytes = env.ext.get::<Win32>().codec.from_utf16(0, &output).0;
                let count = bytes.len();
                (bytes, count)
            };
            if bytes.len() > MAX_BYTES {
                return Err(Failure::Error(234));
            }
            bytes.extend_from_slice(if wide { &[0, 0] } else { &[0] });
            if flags & ALLOCATE_BUFFER != 0 {
                check_range(env.cpu, buffer, 4, Prot::WRITE).map_err(|_| Failure::Error(998))?;
                let minimum = size
                    .checked_mul(if wide { 2 } else { 1 })
                    .ok_or(Failure::Error(8))?;
                let pointer = allocate(env, &bytes, minimum)?;
                if env.cpu.write_u32(buffer, pointer).is_err() {
                    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
                    let _ = globals.free(
                        &mut env.sys.objects,
                        heaps,
                        env.cpu,
                        &mut env.sys.vm,
                        pointer,
                        false,
                    );
                    return Err(Failure::Error(998));
                }
            } else {
                if count >= size as usize {
                    return Err(Failure::Error(122));
                }
                check_range(env.cpu, buffer, bytes.len() as u32, Prot::WRITE)
                    .map_err(|_| Failure::Error(998))?;
                env.cpu
                    .write(buffer, &bytes)
                    .map_err(|_| Failure::Error(998))?;
            }
            Ok(count as u32)
        })();
    match result {
        Ok(count) => count,
        Err(error) => failed(env, wide, error),
    }
}
