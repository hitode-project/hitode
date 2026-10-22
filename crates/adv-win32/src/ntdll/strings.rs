//! Counted RTL strings borrow guest buffers and share invariant UTF-16 casing.
use adv_abi::{AnsiStr, GuestPtr, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, MemFault, Prot};
use adv_machine::Env;

pub(super) fn register(registry: &mut Registry) {
    registry.register(equal_ABI);
    registry.register(init_ansi_ABI);
    registry.register(ansi_to_unicode_ABI);
}

fn descriptor(env: &Env<'_>, pointer: GuestPtr<()>) -> Result<(u16, u32), MemFault> {
    check_range(env.cpu, pointer.addr(), 8, Prot::READ)?;
    Ok((
        env.cpu.read_u16(pointer.addr())?,
        env.cpu.read_u32(pointer.addr() + 4)?,
    ))
}

#[win32(ntdll, name = "RtlEqualUnicodeString", level = implement)]
fn equal(
    env: &mut Env<'_>,
    first: GuestPtr<()>,
    second: GuestPtr<()>,
    insensitive: u32,
) -> Result<u32, MemFault> {
    let (length, a) = descriptor(env, first)?;
    let (other_length, b) = descriptor(env, second)?;
    if length != other_length {
        return Ok(0);
    }
    if length == 0 {
        return Ok(1);
    }
    if length & 1 != 0 {
        warn_unavailable(
            env,
            "ntdll.dll!RtlEqualUnicodeString",
            "odd-byte Unicode string comparison is unavailable",
        );
        return Ok(0);
    }
    check_range(env.cpu, a, u32::from(length), Prot::READ)?;
    check_range(env.cpu, b, u32::from(length), Prot::READ)?;
    let mut first = vec![0; usize::from(length)];
    let mut second = vec![0; usize::from(length)];
    env.cpu.read(a, &mut first)?;
    env.cpu.read(b, &mut second)?;
    let case = |bytes: &[u8; 2]| {
        let unit = u16::from_le_bytes(*bytes);
        if insensitive & 0xff != 0 {
            adv_os::nls::unit_case(unit, true)
        } else {
            unit
        }
    };
    Ok(u32::from(
        first
            .as_chunks::<2>()
            .0
            .iter()
            .zip(second.as_chunks::<2>().0)
            .all(|(a, b)| case(a) == case(b)),
    ))
}

#[win32(ntdll, name = "RtlInitAnsiString", level = implement)]
fn init_ansi(env: &mut Env<'_>, output: GuestPtr<()>, source: AnsiStr) -> Result<(), MemFault> {
    check_range(env.cpu, output.addr(), 8, Prot::WRITE)?;
    let mut descriptor = [0; 8];
    if !source.0.is_null() {
        let length = source.read(env.cpu, 65535)?.len().min(65534) as u16;
        descriptor[..2].copy_from_slice(&length.to_le_bytes());
        descriptor[2..4].copy_from_slice(&(length + 1).to_le_bytes());
        descriptor[4..].copy_from_slice(&source.0.addr().to_le_bytes());
    }
    env.cpu.write(output.addr(), &descriptor)
}

#[win32(ntdll, name = "RtlAnsiStringToUnicodeString", level = implement)]
fn ansi_to_unicode(
    env: &mut Env<'_>,
    destination: GuestPtr<()>,
    source: GuestPtr<()>,
    allocate: u32,
) -> i32 {
    let result = (|| -> Result<(), u32> {
        const ACCESS_VIOLATION: u32 = 0xc0000005;
        check_range(env.cpu, destination.addr(), 8, Prot::RW).map_err(|_| ACCESS_VIOLATION)?;
        let (length, input) = descriptor(env, source).map_err(|_| ACCESS_VIOLATION)?;
        check_range(env.cpu, input, u32::from(length), Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
        let mut bytes = vec![0; usize::from(length)];
        if length != 0 {
            env.cpu
                .read(input, &mut bytes)
                .map_err(|_| ACCESS_VIOLATION)?;
        }
        let units = env.ext.get::<crate::Win32>().codec.to_utf16(0, &bytes);
        let needed = units.len() * 2 + 2;
        if needed > usize::from(u16::MAX) {
            return Err(0xc00000f0);
        }
        let output = units
            .into_iter()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let mut record = [0; 8];
        env.cpu
            .read(destination.addr(), &mut record)
            .map_err(|_| ACCESS_VIOLATION)?;
        let heap = env.ext.get::<adv_os::Heaps>().process_heap;
        let allocated = allocate & 0xff != 0;
        let pointer = if allocated {
            env.ext
                .get_mut::<adv_os::Heaps>()
                .alloc(env.cpu, &mut env.sys.vm, heap, needed as u32, false)
                .ok_or(0xc0000017u32)?
        } else {
            let capacity = u16::from_le_bytes(record[2..4].try_into().unwrap());
            if usize::from(capacity) < needed {
                return Err(0x80000005);
            }
            u32::from_le_bytes(record[4..].try_into().unwrap())
        };
        if check_range(env.cpu, pointer, needed as u32, Prot::WRITE).is_err()
            || env.cpu.write(pointer, &output).is_err()
        {
            if allocated {
                env.ext
                    .get_mut::<adv_os::Heaps>()
                    .free(env.cpu, &mut env.sys.vm, heap, pointer);
            }
            return Err(ACCESS_VIOLATION);
        }
        record[..2].copy_from_slice(&((needed - 2) as u16).to_le_bytes());
        if allocated {
            record[2..4].copy_from_slice(&(needed as u16).to_le_bytes());
            record[4..].copy_from_slice(&pointer.to_le_bytes());
        }
        // Copy the source before publishing the descriptor, including when
        // the two descriptors alias. Counted NULs are data, never terminators.
        env.cpu
            .write(destination.addr(), &record)
            .map_err(|_| ACCESS_VIOLATION)?;
        Ok(())
    })();
    result.err().unwrap_or(0) as i32
}
