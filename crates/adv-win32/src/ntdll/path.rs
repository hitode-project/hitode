//! DOS to NT names share the process VFS cwd and process-heap RTL ownership.
use adv_abi::{GuestPtr, Registry, WideStr, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::Prot;
use adv_machine::Env;
use adv_os::Heaps;

use crate::Win32;

pub(super) fn register(registry: &mut Registry) {
    registry.register(convert_ABI);
}

fn nt_name(env: &Env<'_>, input: &str) -> (String, Option<usize>) {
    // Extended paths preserve dot components/trailing dots and spaces. The
    // already-NT prefix and local device prefix use the same DOS namespace.
    for prefix in [r"\\?\", r"\\.\", r"\??\"] {
        if let Some(path) = input.strip_prefix(prefix) {
            return (format!(r"\??\{path}"), None);
        }
    }
    let vfs = &env.ext.get::<Win32>().vfs;
    let full = vfs.normalize(input);
    let name = if let Some(unc) = full.strip_prefix(r"\\") {
        format!(r"\??\UNC\{unc}")
    } else {
        format!(r"\??\{full}")
    };
    // Only a name originally relative to the current DOS directory can use
    // its relative view. It contains neither a host directory handle nor a
    // second allocation; the native U wrapper captures no CurDirRef.
    let relative_input = !input.starts_with(['\\', '/'])
        && (input.as_bytes().get(1) != Some(&b':')
            || !input
                .as_bytes()
                .get(2)
                .is_some_and(|b| matches!(b, b'\\' | b'/')));
    let relative = if relative_input {
        let cwd = vfs.normalize(&vfs.cwd);
        let cwd = if let Some(unc) = cwd.strip_prefix(r"\\") {
            format!(r"\??\UNC\{}\", unc.trim_end_matches('\\'))
        } else {
            format!(r"\??\{}\", cwd.trim_end_matches('\\'))
        };
        name.get(..cwd.len())
            .filter(|prefix| prefix.eq_ignore_ascii_case(&cwd))
            .filter(|_| name.len() > cwd.len())
            .map(|_| cwd.encode_utf16().count())
    } else {
        None
    };
    (name, relative)
}

fn unicode_string(length: u16, maximum: u16, buffer: u32) -> [u8; 8] {
    let mut descriptor = [0; 8];
    descriptor[..2].copy_from_slice(&length.to_le_bytes());
    descriptor[2..4].copy_from_slice(&maximum.to_le_bytes());
    descriptor[4..].copy_from_slice(&buffer.to_le_bytes());
    descriptor
}

#[win32(ntdll, name = "RtlDosPathNameToNtPathName_U", level = implement)]
fn convert(
    env: &mut Env<'_>,
    input: WideStr,
    output: GuestPtr<()>,
    file_part: GuestPtr<u32>,
    relative_name: GuestPtr<()>,
) -> u32 {
    // BOOLEAN is returned in AL. A failed conversion changes none of the
    // descriptors/pointers and never reports errors through LastError.
    if check_range(env.cpu, output.addr(), 8, Prot::WRITE).is_err()
        || (!file_part.is_null() && check_range(env.cpu, file_part.addr(), 4, Prot::WRITE).is_err())
        || (!relative_name.is_null()
            && check_range(env.cpu, relative_name.addr(), 16, Prot::WRITE).is_err())
    {
        return 0;
    }
    let Ok(units) = input.read(env.cpu, 32768) else {
        return 0;
    };
    if units.is_empty() {
        return 0;
    }
    let Ok(input) = String::from_utf16(&units) else {
        warn_unavailable(
            env,
            "ntdll.dll!RtlDosPathNameToNtPathName_U",
            "unpaired UTF-16 surrogate path is unavailable",
        );
        return 0;
    };
    let (path, relative) = nt_name(env, &input);
    let units: Vec<u16> = path.encode_utf16().collect();
    let Some(length) = units
        .len()
        .checked_mul(2)
        .and_then(|n| u16::try_from(n).ok())
    else {
        return 0;
    };
    let Some(maximum) = length.checked_add(2) else {
        return 0;
    };
    let bytes: Vec<u8> = units
        .iter()
        .copied()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let heap = env.ext.get::<Heaps>().process_heap;
    let Some(buffer) =
        env.ext
            .get_mut::<Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, heap, u32::from(maximum), false)
    else {
        return 0;
    };
    let part = if path.ends_with('\\') {
        0
    } else {
        let prefix = path.rfind('\\').map_or("", |index| &path[..index + 1]);
        buffer + prefix.encode_utf16().count() as u32 * 2
    };
    let mut relative_descriptor = [0; 16];
    if let Some(start) = relative {
        let relative_length = length - start as u16 * 2;
        relative_descriptor[..8].copy_from_slice(&unicode_string(
            relative_length,
            relative_length,
            buffer + start as u32 * 2,
        ));
    }
    let descriptor = unicode_string(length, maximum, buffer);
    let written = env
        .cpu
        .write(buffer, &bytes)
        .and_then(|()| env.cpu.write(output.addr(), &descriptor))
        .and_then(|()| {
            if file_part.is_null() {
                Ok(())
            } else {
                file_part.write(env.cpu, part)
            }
        })
        .and_then(|()| {
            if relative_name.is_null() {
                Ok(())
            } else {
                env.cpu.write(relative_name.addr(), &relative_descriptor)
            }
        });
    if written.is_err() {
        env.ext
            .get_mut::<Heaps>()
            .free(env.cpu, &mut env.sys.vm, heap, buffer);
        return 0;
    }
    1
}
