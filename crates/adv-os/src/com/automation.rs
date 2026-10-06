//! Automation string allocations shared by every COM personality frontend.
use std::collections::BTreeSet;

use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;

/// The same bound used by the guest SysAllocString family.
pub const MAX_BSTR_BYTES: u32 = 16 * 1024 * 1024;

/// Live BSTR data pointers, backed by the process's common heap. The four-byte
/// byte-length prefix and trailing UTF-16 NUL remain in guest memory.
#[derive(Default)]
pub struct AutomationStrings {
    allocations: BTreeSet<u32>,
}

/// Whether a frontend allocated this non-NULL BSTR through the shared owner.
pub fn owns_bstr(env: &Env<'_>, pointer: u32) -> bool {
    env.ext
        .try_get::<AutomationStrings>()
        .is_some_and(|strings| strings.allocations.contains(&pointer))
}

fn allocate(
    env: &mut Env<'_>,
    bytes: u32,
    write_contents: impl FnOnce(&mut Env<'_>, u32) -> Option<()>,
) -> u32 {
    if bytes > MAX_BSTR_BYTES {
        return 0;
    }
    let Some(base) =
        env.ext
            .get_mut::<crate::Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, 0, bytes + 6, false)
    else {
        return 0;
    };
    let result = (|| {
        let pointer = base.checked_add(4)?;
        let end = pointer.checked_add(bytes)?;
        env.cpu.write_u32(base, bytes).ok()?;
        write_contents(env, pointer)?;
        env.cpu.write(end, &[0, 0]).ok()?;
        Some(pointer)
    })();
    let Some(pointer) = result else {
        env.ext
            .get_mut::<crate::Heaps>()
            .free(env.cpu, &mut env.sys.vm, 0, base);
        return 0;
    };
    env.ext
        .get_or_default::<AutomationStrings>()
        .allocations
        .insert(pointer);
    pointer
}

/// Allocate an owned BSTR from host-side UTF-16 code units. The input excludes
/// the terminator; embedded NULs count toward its length. Zero means failure.
/// Publication failure must retire the returned pointer with [`free_bstr`].
pub fn alloc_bstr(env: &mut Env<'_>, text: &[u16]) -> u32 {
    let Some(bytes) = u32::try_from(text.len())
        .ok()
        .and_then(|length| length.checked_mul(2))
    else {
        return 0;
    };
    allocate(env, bytes, |env, pointer| {
        let mut buffer = [0; 4096];
        let mut offset = 0;
        for chunk in text.chunks(buffer.len() / 2) {
            for (index, unit) in chunk.iter().enumerate() {
                buffer[index * 2..index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
            }
            let bytes = chunk.len() * 2;
            env.cpu.write(pointer + offset, &buffer[..bytes]).ok()?;
            offset += bytes as u32;
        }
        Some(())
    })
}

/// SysAllocString's common counted allocation, including odd byte lengths for
/// SysAllocStringByteLen. A NULL source reserves uninitialized data as before.
/// No owner is published if reading or writing the string fails.
pub fn copy_bstr(env: &mut Env<'_>, source: u32, bytes: u32) -> u32 {
    if bytes > MAX_BSTR_BYTES || (source != 0 && !env.cpu.accessible(source, bytes, Prot::READ)) {
        return 0;
    }
    allocate(env, bytes, |env, pointer| {
        if source != 0 {
            let mut buffer = [0; 4096];
            let mut offset = 0;
            while offset < bytes {
                let length = (bytes - offset).min(buffer.len() as u32);
                env.cpu
                    .read(source + offset, &mut buffer[..length as usize])
                    .ok()?;
                env.cpu
                    .write(pointer + offset, &buffer[..length as usize])
                    .ok()?;
                offset += length;
            }
        }
        Some(())
    })
}

/// Retire a shared BSTR exactly once. NULL succeeds; a foreign pointer returns
/// false without freeing another allocation. No LastError or COM state changes.
pub fn free_bstr(env: &mut Env<'_>, pointer: u32) -> bool {
    if pointer == 0 {
        return true;
    }
    if !env
        .ext
        .get_or_default::<AutomationStrings>()
        .allocations
        .remove(&pointer)
    {
        return false;
    }
    env.ext
        .get_mut::<crate::Heaps>()
        .free(env.cpu, &mut env.sys.vm, 0, pointer - 4);
    true
}
